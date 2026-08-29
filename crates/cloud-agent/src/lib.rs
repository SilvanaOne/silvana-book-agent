//! Orderbook Cloud Agent — runs without direct Canton ledger access
//!
//! All ledger operations are proxied through the orderbook gRPC service's
//! DAppProviderService (CIP-0103). The agent signs transaction hashes locally
//! with its Ed25519 private key — the key never leaves the agent.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use clap::Subcommand;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::runner::{AgentOptions, BalanceProvider, run_agent};
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{
    AcceptCip56Params, ExecuteMultiCallParams, FaucetInstrument, FaucetRequest, PreapprovalInfo,
    GetAgentConfigRequest, GetAgentConfigResponse, GetOnboardingStatusRequest, LockHoldingsParams,
    McBatchPay, McPaymentTarget, MessageSignature, MultiCallOp,
    OnboardingStatus as ProtoOnboardingStatus, PrepareTransactionRequest, PrepayTrafficParams,
    ProcessLockUnlockRequestsParams, RegisterAgentRequest, RequestPreapprovalParams,
    RequestRecurringPayasyougoParams, RequestRecurringPrepaidParams, RequestUserServiceParams,
    ResizeLockParams, SplitCcParams, SubmitOnboardingSignatureRequest, TerminateLockParams,
    TokenBalance, TransactionOperation, TransferCcParams, TransferCip56Params,
    VotingAllocation as ProtoVotingAllocation, VotingRequest as ProtoVotingRequest,
    d_app_provider_service_client::DAppProviderServiceClient, prepare_transaction_request::Params,
};
use tx_verifier::OperationExpectation;

pub mod accept_settle;
pub mod acs_worker;
pub mod atomic_swap;
pub mod backend;
pub mod config;
pub mod dvp_gc_worker;
pub mod fill_loop;
pub mod holdings_cache;
pub mod ledger_client;
pub mod merge_worker;
pub mod payment_queue;
pub mod rfq_handler;
pub mod rfq_v2;
pub mod split_worker;
pub mod ticket_pool;
pub mod topup;
pub mod updates_worker;
pub mod venue_registry;

pub use accept_settle::MulticallSettler;
pub use atomic_swap::AtomicSwapper;
pub use backend::CloudSettlementBackend;
pub use holdings_cache::HoldingsCache;
pub use ledger_client::{AtomicProviderClient, DAppProviderClient};

/// Off-chain prepaid traffic balance + credit ceiling, parsed from the
/// `GetPrepaidTrafficBalance` RPC response into typed Decimals.
#[derive(Debug, Clone)]
pub struct PrepaidTrafficBalance {
    pub balance_cc: rust_decimal::Decimal,
    pub credit_limit_cc: rust_decimal::Decimal,
    pub available_cc: rust_decimal::Decimal,
    pub total_credited_cc: rust_decimal::Decimal,
    pub total_debited_cc: rust_decimal::Decimal,
}

// ============================================================================
// Subcommand enums (used by CLI and as function parameters)
// ============================================================================

#[derive(Subcommand)]
pub enum InfoCommands {
    /// List active contracts
    List {
        /// Filter by template ID
        #[arg(long)]
        template: Option<String>,
        /// Show count only
        #[arg(long)]
        count: bool,
    },
    /// Show token balances
    Balance,
    /// Show off-chain prepaid traffic balance + credit limit
    PrepaidTraffic,
    /// Show party identity (party ID, public key, node name)
    Party,
    /// Show network info (DSO party, rates, mining rounds)
    Network,
}

#[derive(Subcommand)]
pub enum PreapprovalCommands {
    /// Create a CIP-56 TransferPreapproval
    Request {
        /// Instrument admin party (DSO for CC, registrar for tokens)
        #[arg(long)]
        instrument_admin: String,
        /// Create even when a preapproval for this admin already exists
        /// (use after an operator rotation)
        #[arg(long)]
        replace: bool,
    },
    /// Fetch existing preapprovals
    Fetch,
    /// Compare held preapprovals against the server's preapproval targets
    Check,
    /// Create any CIP-56 preapproval the server advertises and this party lacks
    Sync,
}

#[derive(Subcommand)]
pub enum TransferCommands {
    /// Send Canton Coin (CC) to another party
    SendCc {
        /// Receiver party ID
        #[arg(long)]
        receiver: String,
        /// Amount to send (decimal string)
        #[arg(long)]
        amount: String,
        /// Optional description
        #[arg(long)]
        description: Option<String>,
        /// Set memo to a generated UUIDv7 (overrides --description)
        #[arg(long)]
        memo_uuid: bool,
    },
    /// Send a CIP-56 instrument token. Completes in one step when the receiver
    /// holds a preapproval for the registry, else creates a TransferOffer.
    SendCip56 {
        /// Receiver party ID
        #[arg(long)]
        receiver: String,
        /// Instrument name or on-chain wire id (e.g. "USDC", "CBTC")
        #[arg(long)]
        instrument_id: String,
        /// Registrar party ID (InstrumentId.admin / instrument source).
        /// Defaults to the registry recorded for the instrument.
        #[arg(long)]
        instrument_admin: Option<String>,
        /// Amount to send (decimal string)
        #[arg(long)]
        amount: String,
        /// Optional transfer reference
        #[arg(long)]
        reference: Option<String>,
        /// Use at most this many holdings as inputs, biggest first. Defaults to
        /// the amulet protocol limit for CC and no cap for other instruments.
        #[arg(long)]
        count: Option<u32>,
    },
    /// Accept an incoming CIP-56 TransferOffer
    AcceptCip56 {
        /// TransferOffer contract ID to accept
        #[arg(long)]
        contract_id: String,
    },
    /// Split CC into multiple amulets (MergeSplit via AmuletRules_Transfer)
    SplitCc {
        /// Comma-separated output amounts (e.g. "10.5,20.0,5.0")
        #[arg(long, value_delimiter = ',')]
        output_amounts: Vec<String>,
        /// Comma-separated input amulet contract IDs
        #[arg(long, value_delimiter = ',')]
        amulet_cids: Vec<String>,
    },
    /// Batch pay CC to multiple recipients atomically via MultiCall
    BatchPay {
        /// Recipient party IDs (repeat for each recipient)
        #[arg(long = "receiver")]
        receivers: Vec<String>,
        /// Amounts in CC (repeat for each recipient, same order as --receiver)
        #[arg(long = "amount")]
        amounts: Vec<String>,
        /// CSV file with columns: party_id, amount (overrides --receiver/--amount)
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// Description applied to all payments
        #[arg(long)]
        description: Option<String>,
        /// Comma-separated input amulet contract IDs (optional — selected
        /// automatically from the largest unlocked amulets when omitted)
        #[arg(long, value_delimiter = ',')]
        amulet_cids: Option<Vec<String>>,
    },
    /// Top up the off-chain prepaid traffic balance by transferring CC to PARTY_PREPAID_TRAFFIC
    PrepayTraffic {
        /// CC amount to credit (decimal string)
        #[arg(long)]
        amount: String,
        /// Optional human-readable note
        #[arg(long)]
        description: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SignCommands {
    /// Sign a 34-byte multihash (Canton format) using Ed25519 key
    Multihash {
        /// Base64-encoded 34-byte multihash
        #[arg(long)]
        input: String,
        /// Private key (base58). Defaults to the configured agent key
        #[arg(long)]
        private_key: Option<String>,
    },
    /// Sign a text message using Ed25519 key
    Message {
        /// Text message to sign (signed as UTF-8 bytes)
        #[arg(long)]
        input: String,
        /// Private key (base58). Defaults to the configured agent key
        #[arg(long)]
        private_key: Option<String>,
    },
    /// Sign binary data using Ed25519 key
    Binary {
        /// Hex-encoded binary data (with or without 0x prefix)
        #[arg(long)]
        input: String,
        /// Private key (base58). Defaults to the configured agent key
        #[arg(long)]
        private_key: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum UserServiceCommands {
    /// Request UserService for this party (one-time onboarding setup)
    Request {
        /// Optional reference ID for transaction tracking
        #[arg(long)]
        reference_id: Option<String>,
        /// Human-readable party name (optional)
        #[arg(long)]
        party_name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SubscriptionCommands {
    /// Request a prepaid recurring payment
    RequestPrepaid {
        /// Amount per day in USD
        #[arg(long, default_value = "0.033")]
        amount: String,
        /// Total limit in USD
        #[arg(long, default_value = "12")]
        limit: String,
        /// Required locked amount in USD
        #[arg(long, default_value = "1")]
        locked_amount: String,
        /// Lock duration in days
        #[arg(long, default_value = "90")]
        lock_days: u32,
        /// Payment description
        #[arg(long, default_value = "Subscription")]
        description: Option<String>,
        /// External reference
        #[arg(long)]
        reference: Option<String>,
    },
    /// Request a pay-as-you-go recurring payment
    RequestPayasyougo {
        /// App party ID (recipient, defaults to settlement operator)
        #[arg(long)]
        app: Option<String>,
        /// Payment amount in USD
        #[arg(long, default_value = "0.033")]
        amount: String,
        /// Payment description
        #[arg(long, default_value = "Subscription")]
        description: Option<String>,
        /// External reference
        #[arg(long)]
        reference: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum LockCommands {
    /// Lock holdings via LockService.LockHoldings
    Holdings {
        /// LockService contract ID
        #[arg(long)]
        lock_service_cid: String,
        /// LockService template ID
        #[arg(long)]
        lock_service_template_id: String,
        /// LockService created_event_blob (for disclosure)
        #[arg(long)]
        lock_service_blob: String,
        /// Amount to lock
        #[arg(long)]
        amount: String,
        /// Instrument token ID (e.g., "HECTO")
        #[arg(long)]
        instrument_id: String,
        /// Lock context (UUIDv7; generated if omitted)
        #[arg(long)]
        context: Option<String>,
        /// JSON file with initial voting allocations (array of {context, amount})
        #[arg(long)]
        allocations_file: Option<String>,
        /// Fee recipient party IDs
        #[arg(long, num_args = 1..)]
        fee_parties: Vec<String>,
        /// Fee amounts (one per fee party)
        #[arg(long, num_args = 1..)]
        fee_amounts: Vec<String>,
        /// Amulet contract IDs for fee payment
        #[arg(long, num_args = 1..)]
        amulet_cids: Vec<String>,
    },
    /// Process voting lock/unlock requests on LockController
    Vote {
        /// LockController contract ID
        #[arg(long)]
        lock_controller_cid: String,
        /// LockController template ID
        #[arg(long)]
        lock_controller_template_id: String,
        /// JSON file with voting requests (array of {context, amount, direction})
        #[arg(long)]
        requests_file: String,
        /// Fee recipient party IDs
        #[arg(long, num_args = 1..)]
        fee_parties: Vec<String>,
        /// Fee amounts (one per fee party)
        #[arg(long, num_args = 1..)]
        fee_amounts: Vec<String>,
        /// Amulet contract IDs for fee payment
        #[arg(long, num_args = 1..)]
        amulet_cids: Vec<String>,
    },
    /// Resize lock amount on LockController
    Resize {
        /// LockController contract ID
        #[arg(long)]
        lock_controller_cid: String,
        /// LockController template ID
        #[arg(long)]
        lock_controller_template_id: String,
        /// New lock amount
        #[arg(long)]
        new_amount: String,
        /// Instrument token ID (for querying additional holdings)
        #[arg(long)]
        instrument_id: String,
        /// JSON file with voting requests to apply atomically (optional)
        #[arg(long)]
        requests_file: Option<String>,
        /// Fee recipient party IDs
        #[arg(long, num_args = 1..)]
        fee_parties: Vec<String>,
        /// Fee amounts (one per fee party)
        #[arg(long, num_args = 1..)]
        fee_amounts: Vec<String>,
        /// Amulet contract IDs for fee payment
        #[arg(long, num_args = 1..)]
        amulet_cids: Vec<String>,
    },
    /// Terminate lock on LockController (requires all allocations unlocked)
    Terminate {
        /// LockController contract ID
        #[arg(long)]
        lock_controller_cid: String,
        /// LockController template ID
        #[arg(long)]
        lock_controller_template_id: String,
        /// Fee recipient party IDs
        #[arg(long, num_args = 1..)]
        fee_parties: Vec<String>,
        /// Fee amounts (one per fee party)
        #[arg(long, num_args = 1..)]
        fee_amounts: Vec<String>,
        /// Amulet contract IDs for fee payment
        #[arg(long, num_args = 1..)]
        amulet_cids: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum FaucetCommands {
    /// Request tokens from faucet
    Get {
        /// Token name. "Amulet" or "CC" → Canton Coin (admin defaults to DSO).
        /// Anything else is treated as a CIP-56 token and requires --admin.
        #[arg(long)]
        token: String,
        /// Token admin party. Optional:
        ///   - Canton Coin / Amulet: defaults to $DSO.
        ///   - CIP-56: defaults to the registry fetched from orderbook-rpc
        ///     (`GetInstruments`); pass explicitly to override or when the
        ///     token is not registered.
        #[arg(long)]
        admin: Option<String>,
        /// Faucet ticket (devnet: any string)
        #[arg(long, default_value = "devnet")]
        ticket: String,
        /// Requested amount (optional — server decides default)
        #[arg(long)]
        amount: Option<String>,
        /// Dry run — check if request would succeed without executing
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum AtomicCommands {
    /// Generate (or verify) the secp256k1 quote-signing key — prints the
    /// ATOMIC_QUOTE_PRIVATE_KEY line for .env; never writes a file
    Keygen,
    /// AtomicDVP venue operations (one venue per LP per market pair)
    Venue {
        #[command(subcommand)]
        command: AtomicVenueCommands,
    },
    /// SettlementTicket operations
    Tickets {
        #[command(subcommand)]
        command: AtomicTicketCommands,
    },
    /// Split own holdings into pre-set denominations
    Split {
        /// Market ID (e.g. "CC-USDC")
        #[arg(long)]
        market: String,
        /// Which leg to split: "base" | "quote"
        #[arg(long)]
        instrument: String,
        /// Split specs, comma-separated "AMOUNTxCOUNT" (e.g. "10x20,100x5")
        #[arg(long)]
        splits: String,
    },
    /// Read-only status: venues (+ key match), live tickets, denomination histogram
    Status {
        /// Restrict to one market (pairName)
        #[arg(long)]
        market: Option<String>,
    },
    /// Orchestrate LP setup: key check → service-file check → venues →
    /// receiving preapprovals → ticket batch → denomination splits
    Setup {
        /// AtomicDVPService disclosure JSON exported by the provider
        /// (`orderbook atomic export --file …`)
        #[arg(long, default_value = "atomic-dvp-service.json")]
        service_file: String,
    },
}

#[derive(Subcommand)]
pub enum AtomicVenueCommands {
    /// Create the AtomicDVP venue for a market pair (key from ATOMIC_QUOTE_PRIVATE_KEY)
    Create {
        /// Market ID (pairName), format "BASE-QUOTE"
        #[arg(long)]
        market: String,
    },
    /// List this LP's AtomicDVP venues as JSON (contract ids + instrument admins)
    List,
    /// Rotate the venue's quote key to the current ATOMIC_QUOTE_PRIVATE_KEY —
    /// KILLS all live envelopes + old-key signatures
    RotateKey {
        /// Market ID (pairName)
        #[arg(long)]
        market: String,
    },
    /// Retire (archive) this LP's AtomicDVP venue for a market pair. Use before
    /// re-creating a venue under a changed instrument registrar. KILLS every
    /// live signed envelope for the pair.
    Retire {
        /// Market ID (pairName)
        #[arg(long)]
        market: String,
    },
}

#[derive(Subcommand)]
pub enum AtomicTicketCommands {
    /// Issue a batch of SettlementTickets (client-generated UUIDv7 ids)
    Issue {
        /// Number of tickets to issue
        #[arg(long)]
        count: u32,
    },
    /// Cancel tickets by contract id, or all live tickets with --all-free
    Cancel {
        /// Ticket contract IDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        cids: Vec<String>,
        /// Cancel ALL live tickets owned by this LP
        #[arg(long)]
        all_free: bool,
    },
}

// ============================================================================
// Library functions
// ============================================================================

/// Fetch instrument registry (CC/Amulet + DSO, CIP-56 registries) from
/// orderbook-rpc and populate `BaseConfig`. Replaces the old configuration.toml
/// `[[canton_coin]]` / `[[instrument]]` sections for the client.
pub async fn populate_instruments(config: &mut BaseConfig) -> Result<()> {
    let mut client = agent_logic::client::OrderbookClient::new(config)
        .await
        .context("Failed to create orderbook client for instrument registry fetch")?;
    let instruments = client
        .get_instruments()
        .await
        .context("Failed to fetch instruments from orderbook-rpc")?;
    config.populate_instruments_from_rpc(instruments);
    Ok(())
}

// ============================================================================
// Agent mode
// ============================================================================

/// Balance provider that fetches holdings via DAppProviderService gRPC proxy
pub struct CloudBalanceProvider {
    pub client: TokioMutex<DAppProviderClient>,
}

#[async_trait]
impl BalanceProvider for CloudBalanceProvider {
    async fn fetch_balances(&self) -> Result<Vec<TokenBalance>> {
        self.client.lock().await.get_balances().await
    }
}

pub async fn run_cloud_agent(
    config: BaseConfig,
    settlement_only: bool,
    orders_only: bool,
    no_restore: bool,
    no_reject: bool,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    version_info: Option<&str>,
) -> Result<()> {
    if let Some(v) = version_info {
        info!("Starting Orderbook Cloud Agent (build: {})", v);
    } else {
        info!("Starting Orderbook Cloud Agent");
    }
    info!("Party ID: {}", config.party_id);
    info!("Orderbook URL: {}", config.orderbook_grpc_url);
    info!(
        "Fee reserve: {:.2} CC (traffic billing handled off-chain by ledger)",
        config.fee_reserve_cc
    );

    // Ensure the error reporter is installed for the production agent path,
    // for library embedders that bypass the CLI. Idempotent.
    agent_logic::error_reporter::init_from_config(&config);

    if config.rfq_v2_only && orders_only {
        anyhow::bail!(
            "--orders-only conflicts with rfq_v2_only = true (agent.toml / RFQ_V2_ONLY): \
             v2-only mode disables order placement, so the agent would do nothing"
        );
    }

    // Create LiquidityManager early so it can be shared with RfqHandler and Backend
    let liquidity_manager = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    // 0 disables the balance-staleness gate.
    liquidity_manager.set_stale_after(if config.balance_stale_after_secs == 0 {
        std::time::Duration::MAX
    } else {
        std::time::Duration::from_secs(config.balance_stale_after_secs)
    });

    // Register token aliases from server market data (symbol → instrument_id).
    // Also capture market_id → (base_instrument, quote_instrument) for the
    // RFQ V2 venue validation / split targets.
    let mut market_instrument_ids: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    {
        let mut client = agent_logic::client::OrderbookClient::new(&config).await?;
        match client.get_markets().await {
            Ok(markets) => {
                for market in &markets {
                    let parts: Vec<&str> = market.market_id.split('-').collect();
                    if parts.len() == 2 {
                        // The CC key is owned by its own balance feed; never
                        // alias it to a market instrument.
                        if parts[0] != agent_logic::liquidity::CC_TOKEN {
                            liquidity_manager
                                .register_alias(parts[0], &market.base_instrument)
                                .await;
                        }
                        if parts[1] != agent_logic::liquidity::CC_TOKEN {
                            liquidity_manager
                                .register_alias(parts[1], &market.quote_instrument)
                                .await;
                        }
                    }
                    market_instrument_ids.insert(
                        market.market_id.clone(),
                        (
                            market.base_instrument.clone(),
                            market.quote_instrument.clone(),
                        ),
                    );
                }
                info!("Registered token aliases from {} markets", markets.len());
            }
            Err(e) => {
                tracing::warn!("Failed to fetch markets for alias registration: {}", e);
            }
        }
    }

    // Single Shutdown shared by every background task (LP stream, mid-price
    // poller, ACS / merge / payment-queue / topup workers, runner main loop).
    // The runner's Ctrl-C handler signals it on first Ctrl-C; every loop with
    // a clone wakes immediately.
    let lp_shutdown = Shutdown::new();

    // Issuance forecast poller — keeps the process-global coefficient fresh for
    // the RFQ overload gates, the fee-dispatch pause and the DvpProposal GC.
    // Spawned BEFORE the GC so the GC's first cycle (~2s later) reads a real
    // coefficient rather than the 0.0 default. Idempotent, so the runner may
    // call it again for embedders that bypass this function.
    agent_logic::forecast::spawn_forecast_poller(config.clone(), lp_shutdown.clone());

    // DvpProposal GC — archives expired legacy-DVP proposals during
    // high-issuance-coefficient windows (see dvp_gc_worker.rs). Env-gated;
    // no-op when DVP_GC_ENABLED=false.
    dvp_gc_worker::spawn_dvp_gc_worker(config.clone(), lp_shutdown.clone());

    // RFQ V2 activation: LP-level switch + quote key present (config::assemble
    // validated the pairing already; this is belt-and-braces).
    let rfq_v2_cfg = config
        .liquidity_provider
        .as_ref()
        .and_then(|lp| lp.rfq_v2.clone())
        .filter(|v2| v2.enabled);
    let rfq_v2_active = config.liquidity_provider.is_some()
        && rfq_v2_cfg.is_some()
        && config.atomic_quote_key.is_some();

    // Belt-and-braces for library embedders that build BaseConfig directly and
    // skip validate_v2(): with V1 and orders disabled, an inactive V2 stack
    // means the agent would run while quoting nothing. Unreachable via the CLI
    // path (config::assemble enforces the same requirements).
    if config.rfq_v2_only && !rfq_v2_active {
        anyhow::bail!(
            "rfq_v2_only = true but the RFQ V2 stack is not active — requires a \
             [liquidity_provider] section, [liquidity_provider.rfq_v2].enabled = true \
             and the atomic quote key; nothing to quote, refusing to run"
        );
    }

    // ONE shared multi-instrument holdings pool for RFQ v1 + V2. The splitter
    // reserve (largest holding per instrument) only exists in V2 mode so plain
    // v1 deployments keep their exact selection behavior.
    let holdings_cache = holdings_cache::HoldingsCache::new(rfq_v2_active);

    let mut atomic_v2_snapshot: Option<
        Arc<
            dyn Fn() -> (
                    Vec<agent_logic::state::SavedTicket>,
                    Vec<agent_logic::state::SavedPendingV2>,
                ) + Send
                + Sync,
        >,
    > = None;

    // Captured from the RfqHandler (LP mode only) so the LIQUIDITY heartbeat can
    // bucket the holdings histogram by USD; None when not an LP.
    let mut lp_mid_prices: Option<
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, agent_logic::pool_impact::MarketMid>>>,
    > = None;
    // Trailing net tracker, LP mode only. Created before any pricing so it
    // accumulates even while the config is still in shadow.
    let mut net_positions: Option<Arc<agent_logic::net_position::NetPositionTracker>> = None;
    let quoted_rfq_trades = if config.liquidity_provider.is_some() {
        // Decay window = the max across configured pool_impact sections (the
        // tracker is per-token, so per-market windows cannot differ anyway).
        let window_hours = config
            .markets
            .iter()
            .filter_map(|m| m.rfq.as_ref())
            .filter_map(|r| r.pool_impact.as_ref())
            .map(|p| p.window_hours)
            .fold(f64::NAN, f64::max);
        let window_hours = if window_hours.is_finite() { window_hours } else { 24.0 };
        // Derived from the quote lifetime: a shorter backstop would reverse
        // counts for quotes that are still open.
        let stale_pending_after = config
            .liquidity_provider
            .as_ref()
            .and_then(|lp| lp.rfq_v2.as_ref())
            .map(|v2| v2.stale_pending_after())
            .unwrap_or_else(|| {
                agent_logic::config::RfqV2Config::default().stale_pending_after()
            });
        let tracker = agent_logic::net_position::NetPositionTracker::load_or_new(
            PathBuf::from("net-positions.json"),
            window_hours,
            stale_pending_after,
        );
        net_positions = Some(tracker.clone());

        let mut rfq_handler = rfq_handler::RfqHandler::new(&config)
            .ok_or_else(|| anyhow::anyhow!("Failed to create RFQ handler"))?;
        rfq_handler.set_liquidity_manager(liquidity_manager.clone());
        rfq_handler.set_net_positions(tracker);
        let quoted_trades = rfq_handler.quoted_trades();
        let rfq_handler = Arc::new(rfq_handler);
        lp_mid_prices = Some(rfq_handler.mid_prices());

        // Mid-price poller runs in BOTH modes — V2 pricing and the LIQUIDITY
        // histogram need mid_prices even when the V1 stream never connects.
        let rfq_market_ids: Vec<String> = config
            .markets
            .iter()
            .filter(|m| m.enabled && m.rfq.as_ref().map_or(false, |r| r.enabled))
            .map(|m| m.market_id.clone())
            .collect();
        spawn_mid_price_poller(
            config.clone(),
            rfq_handler.mid_prices(),
            rfq_market_ids,
            lp_shutdown.clone(),
        );

        if config.rfq_v2_only {
            info!(
                "rfq_v2_only = true: V1 LP settlement stream disabled — no V1 LP \
                 registration, no V1 quotes, no grid orders"
            );
        } else {
            info!(
                "LP mode enabled: name={}, starting settlement stream",
                config.liquidity_provider.as_ref().unwrap().name
            );
            let config_clone = config.clone();
            let lp_shutdown_clone = lp_shutdown.clone();
            let handler = rfq_handler.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    run_lp_settlement_stream(config_clone, handler, lp_shutdown_clone).await
                {
                    tracing::error!("LP settlement stream failed: {}", e);
                }
            });
        }

        // ---- RFQ V2 stack (updates watcher, split/ticket maintenance, atomic stream) ----
        if rfq_v2_active {
            let v2cfg = rfq_v2_cfg.clone().expect("checked by rfq_v2_active");
            let quote_key = config
                .atomic_quote_key
                .clone()
                .expect("checked by rfq_v2_active");
            match setup_rfq_v2(
                &config,
                &v2cfg,
                &quote_key,
                &market_instrument_ids,
                rfq_handler.clone(),
                holdings_cache.clone(),
                liquidity_manager.clone(),
                net_positions.clone(),
                no_restore,
                version_info,
                lp_shutdown.clone(),
            )
            .await
            {
                Ok(snapshot) => atomic_v2_snapshot = Some(snapshot),
                Err(e) if config.rfq_v2_only => {
                    // With V1 and orders disabled there is nothing left to
                    // quote — fail loud instead of running a do-nothing agent.
                    return Err(e.context(
                        "RFQ V2 setup failed and rfq_v2_only = true — nothing to quote, refusing to run",
                    ));
                }
                Err(e) => {
                    // Never crash v1 over a V2 wiring failure.
                    tracing::error!(
                        "RFQ V2 setup failed — atomic quoting disabled, v1 continues: {:#}",
                        e
                    );
                }
            }
        }

        Some(quoted_trades)
    } else {
        None
    };

    let confirm_lock = agent_logic::confirm::new_confirm_lock();
    let mut backend = CloudSettlementBackend::new(
        config.clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock,
        liquidity_manager,
        lp_shutdown.clone(),
        holdings_cache,
    );
    if let Some(mp) = lp_mid_prices {
        backend = backend.with_mid_prices(mp);
    }

    // When RFQ v2 is active, setup_rfq_v2 (above) already spawned an
    // all-instrument merge worker. Otherwise (plain v1 LP or non-LP settlement
    // agent) spawn the CC-only merge here — this restores the merge that used to
    // run unconditionally from CloudSettlementBackend::new. Mutually exclusive
    // with the RFQ-v2 spawn, so CC is never merged by two workers at once.
    if config.merge_threshold.is_some() && !rfq_v2_active {
        crate::merge_worker::spawn_merge_worker(
            config.clone(),
            backend.holdings_cache().clone(),
            vec![split_worker::SplitInstrument::cc()],
            lp_shutdown.clone(),
        );
    }

    let ledger_client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
    .context("Failed to create ledger client")?;

    // Auto-topup wiring: build a separate DAppProviderClient for the
    // background topup runner (its own JWT, its own connection — so its
    // submit_transaction never recurses through the main client's
    // post-success hook). Returns None when env vars aren't set, in
    // which case auto-topup is fully disabled.
    let topup_trigger = match (
        config.min_prepaid_traffic_balance_cc,
        config.prepaid_traffic_topup_cc,
    ) {
        (Some(_), Some(_)) => {
            let topup_client = DAppProviderClient::new(
                &config.orderbook_grpc_url,
                &config.party_id,
                &config.role,
                &config.private_key,
                config.token_ttl_secs,
                Some(config.node_name.as_str()),
                &config.ledger_service_public_key,
                Some(config.connection_timeout_secs),
                Some(config.request_timeout_secs),
            )
            .await
            .context("Failed to create topup ledger client")?;
            let runner = topup::TopupRunner::new(
                topup_client,
                config.party_id.clone(),
                config.min_prepaid_traffic_balance_cc,
                config.prepaid_traffic_topup_cc,
            )
            .expect("env vars present (matched above)");
            info!(
                min_cc = %config.min_prepaid_traffic_balance_cc.unwrap(),
                topup_cc = %config.prepaid_traffic_topup_cc.unwrap(),
                "Auto-topup enabled"
            );
            Some(Arc::new(runner).spawn(lp_shutdown.clone()))
        }
        _ => {
            info!(
                "Auto-topup disabled (MIN_PREPAID_TRAFFIC_BALANCE_CC / PREPAID_TRAFFIC_TOPUP_CC not set)"
            );
            None
        }
    };

    let ledger_client = if let Some(trigger) = topup_trigger {
        ledger_client.with_topup_trigger(trigger)
    } else {
        ledger_client
    };

    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };

    // The background balance poller gets its own client so it never contends
    // with the main loop's.
    let poller_ledger_client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
    .context("Failed to create balance poller client")?;
    let poller_balance_provider: std::sync::Arc<dyn agent_logic::runner::BalanceProvider> =
        std::sync::Arc::new(CloudBalanceProvider {
            client: TokioMutex::new(poller_ledger_client),
        });

    let result = run_agent(
        config,
        backend,
        balance_provider,
        AgentOptions {
            settlement_only,
            orders_only,
            poller_balance_provider: Some(poller_balance_provider),
            actionable_count: None,
            shutdown: None,
            accepted_rfq_trades: None,
            rejected_rfq_trades: None,
            quoted_rfq_trades,
            lp_shutdown: Some(lp_shutdown),
            state_file: Some(PathBuf::from("agent-state.json")),
            no_restore,
            fill_state: None,
            no_reject,
            atomic_v2_snapshot,
            net_positions: net_positions.clone(),
        },
    )
    .await;

    // Graceful-shutdown save of the net-position map (checkpoints cover
    // crashes up to 60 s back; this makes a clean stop lossless).
    if let Some(tracker) = &net_positions {
        tracker.save();
    }
    result
}

/// Wire the RFQ V2 stack: ticket pool + venue registry + quote state, restore
/// saved reservations, spawn the updates watcher + maintenance worker, and —
/// when at least one venue validates — the atomic RFQ stream.
///
/// Returns the SavedState snapshot provider for the runner's shutdown save.
/// PUBLIC for library embedders (e.g. a test harness running many agents in
/// one process): given a programmatically-built `BaseConfig` (see
/// `BaseConfig::for_party`) plus the shared per-agent objects, this brings up
/// the entire per-agent RFQ V2 LP stack in one call.
#[allow(clippy::too_many_arguments)]
pub async fn setup_rfq_v2(
    config: &BaseConfig,
    v2cfg: &agent_logic::config::RfqV2Config,
    quote_key: &agent_logic::config::AtomicQuoteKey,
    market_instrument_ids: &std::collections::HashMap<String, (String, String)>,
    rfq_handler: Arc<rfq_handler::RfqHandler>,
    holdings_cache: Arc<holdings_cache::HoldingsCache>,
    liquidity_manager: Arc<agent_logic::liquidity::LiquidityManager>,
    net_positions: Option<Arc<agent_logic::net_position::NetPositionTracker>>,
    no_restore: bool,
    version_info: Option<&str>,
    lp_shutdown: Shutdown,
) -> Result<
    Arc<
        dyn Fn() -> (
                Vec<agent_logic::state::SavedTicket>,
                Vec<agent_logic::state::SavedPendingV2>,
            ) + Send
            + Sync,
    >,
> {
    use holdings_cache::{CC_INSTRUMENT, instrument_key};

    let lp_config = config
        .liquidity_provider
        .as_ref()
        .ok_or_else(|| anyhow!("LP config required for RFQ V2"))?;

    // Per-market V2 metadata: resolved instruments, expected venue shape,
    // split targets.
    let mut market_v2 = std::collections::HashMap::new();
    let mut market_instruments = std::collections::HashMap::new();
    let mut expected_venues = std::collections::HashMap::new();
    // Legacy per-market ladder candidates (instrument, market ladder,
    // default rung) — used only when the global per-instrument map is empty.
    let mut legacy_split_candidates: Vec<(
        split_worker::SplitInstrument,
        Vec<String>,
        Option<(rust_decimal::Decimal, u32)>,
    )> = Vec::new();

    for market in &config.markets {
        if !market.enabled {
            continue;
        }
        let Some(rfq) = market.rfq.as_ref().filter(|r| r.enabled) else {
            continue;
        };
        let Some(v2m) = rfq.v2.as_ref().filter(|v| v.enabled) else {
            continue;
        };

        // Orderbook instrument ids: server market data, else market_id split.
        let (base_instr, quote_instr) = market_instrument_ids
            .get(&market.market_id)
            .cloned()
            .unwrap_or_else(|| {
                let parts: Vec<&str> = market.market_id.split('-').collect();
                if parts.len() == 2 {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    (market.market_id.clone(), String::new())
                }
            });
        let (base_ocid, base_admin) = config.resolve_instrument(&base_instr);
        let (quote_ocid, quote_admin) = config.resolve_instrument(&quote_instr);
        let base_is_cc = base_ocid == "Amulet";
        let quote_is_cc = quote_ocid == "Amulet";
        let base_key = if base_is_cc {
            CC_INSTRUMENT.to_string()
        } else {
            instrument_key(&base_admin, &base_ocid)
        };
        let quote_key_instr = if quote_is_cc {
            CC_INSTRUMENT.to_string()
        } else {
            instrument_key(&quote_admin, &quote_ocid)
        };

        expected_venues.insert(
            market.market_id.clone(),
            venue_registry::ExpectedVenue {
                base_id: base_ocid.clone(),
                base_admin: base_admin.clone(),
                quote_id: quote_ocid.clone(),
                quote_admin: quote_admin.clone(),
            },
        );
        market_instruments.insert(
            market.market_id.clone(),
            rfq_v2::MarketInstruments {
                base_key: base_key.clone(),
                base_is_cc,
                quote_key: quote_key_instr.clone(),
                quote_is_cc,
            },
        );
        market_v2.insert(market.market_id.clone(), v2m.clone());

        let default_rung = market
            .base_order_size
            .as_ref()
            .and_then(|s| s.parse::<rust_decimal::Decimal>().ok())
            .map(|amt| (amt, lp_config.max_concurrent_rfqs as u32));
        legacy_split_candidates.push((
            split_worker::SplitInstrument {
                key: base_key,
                is_cc: base_is_cc,
                on_chain_id: base_ocid,
                admin: base_admin,
            },
            v2m.denominations.clone(),
            default_rung,
        ));
        legacy_split_candidates.push((
            split_worker::SplitInstrument {
                key: quote_key_instr,
                is_cc: quote_is_cc,
                on_chain_id: quote_ocid,
                admin: quote_admin,
            },
            v2m.denominations.clone(),
            default_rung,
        ));
    }

    // Split targets: ONE ladder per instrument. The global
    // [liquidity_provider.rfq_v2.denominations] map wins; without it, fall
    // back to per-market ladders applied to both legs (first market wins per
    // instrument).
    let split_targets: Vec<split_worker::SplitTarget> = if !v2cfg.denominations.is_empty() {
        let mut out = Vec::new();
        for (symbol, ladder) in &v2cfg.denominations {
            let (on_chain_id, admin) = config.resolve_instrument(symbol);
            let is_cc = on_chain_id == "Amulet";
            if !is_cc && admin.is_empty() {
                tracing::warn!(
                    "rfq_v2.denominations: cannot resolve instrument '{}' — ladder ignored",
                    symbol
                );
                continue;
            }
            let key = if is_cc {
                CC_INSTRUMENT.to_string()
            } else {
                instrument_key(&admin, &on_chain_id)
            };
            out.push(split_worker::SplitTarget {
                instrument: split_worker::SplitInstrument {
                    key,
                    is_cc,
                    on_chain_id,
                    admin,
                },
                denominations: ladder.clone(),
            });
        }
        out
    } else {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (instr, denoms, default_rung) in legacy_split_candidates {
            let ladder = if denoms.is_empty() {
                default_rung
                    .map(|(amt, count)| vec![format!("{amt}x{count}")])
                    .unwrap_or_default()
            } else {
                denoms
            };
            if ladder.is_empty() || !seen.insert(instr.key.clone()) {
                continue;
            }
            out.push(split_worker::SplitTarget {
                instrument: instr,
                denominations: ladder,
            });
        }
        out
    };

    // Dust thresholds: an instrument's smallest ladder rung. Holdings below
    // it fund settles dust-first when few enough fit, else get swept into
    // rung-funded settles as extra inputs for consolidation.
    let mut dust_thresholds = std::collections::HashMap::new();
    for t in &split_targets {
        if let Ok(rungs) = split_worker::parse_splits(&t.denominations) {
            if let Some(min) = rungs.iter().map(|(d, _)| *d).min() {
                dust_thresholds.insert(t.instrument.key.clone(), min);
            }
        }
    }
    holdings_cache.set_dust_thresholds(dust_thresholds).await;

    // Dust-merge worker across all ladder instruments (CC + utility). Shares the
    // cache + dust thresholds set above; per instrument it consolidates only
    // sub-rung dust (CC via SplitCc, utility via a CIP-56 self-transfer) and
    // never the rungs the split worker maintains.
    if config.merge_threshold.is_some() {
        let mut merge_instruments: Vec<split_worker::SplitInstrument> =
            split_targets.iter().map(|t| t.instrument.clone()).collect();
        // CC accumulates dust from fee payments even without a CC ladder, so
        // always merge it (legacy total-count fallback when no CC dust threshold).
        if !merge_instruments.iter().any(|i| i.is_cc) {
            merge_instruments.push(split_worker::SplitInstrument::cc());
        }
        crate::merge_worker::spawn_merge_worker(
            config.clone(),
            holdings_cache.clone(),
            merge_instruments,
            lp_shutdown.clone(),
        );
    }

    if market_v2.is_empty() {
        return Err(anyhow!(
            "liquidity_provider.rfq_v2 is enabled but no enabled market has [markets.rfq.v2] enabled"
        ));
    }

    let ticket_pool = v2cfg
        .ticket_threshold_usd
        .is_some()
        .then(|| Arc::new(ticket_pool::TicketPool::new()));
    let venue_registry = Arc::new(venue_registry::VenueRegistry::new(
        config.party_id.clone(),
        quote_key.pub_spki_hex.clone(),
        expected_venues,
    ));

    let mut state = rfq_v2::RfqV2State::new(
        config.party_id.clone(),
        lp_config.name.clone(),
        config.synchronizer_id.clone(),
        quote_key.clone(),
        v2cfg.clone(),
        market_v2,
        market_instruments,
        holdings_cache.clone(),
        ticket_pool.clone(),
        venue_registry.clone(),
        liquidity_manager,
        config.clone(),
        &split_targets,
    )
    .with_mid_prices(rfq_handler.mid_prices());
    if let Some(tracker) = net_positions {
        state = state.with_net_positions(tracker);
    }
    let state = Arc::new(state);

    // RESTORE saved V2 state BEFORE any worker starts: an already-delivered
    // envelope is self-contained (the ledger verifies it), so the reservations
    // backing it must exist before the first ACS refresh could hand those
    // holdings to another selection.
    if !no_restore {
        if let Some(saved) = agent_logic::state::load_state(&PathBuf::from("agent-state.json"))
            .filter(|s| s.party_id == config.party_id)
        {
            if let Some(pool) = &ticket_pool {
                pool.restore(saved.atomic_tickets);
            }
            state.restore_pending(saved.pending_v2_quotes).await;
        }
    }

    // Initial venue discovery + validation
    let mut discovery_client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
    .context("RFQ V2: failed to create venue discovery client")?;
    if let Err(e) = venue_registry.refresh(&mut discovery_client).await {
        tracing::error!("RFQ V2: initial venue refresh failed: {:#}", e);
    }
    let validated = venue_registry.validated_market_ids();

    // Workers exist whenever V2 is enabled (cache freshness, ticket refill,
    // splits) — the quoting stream additionally requires a validated venue.
    let (settle_tx, settle_rx) = tokio::sync::mpsc::unbounded_channel();
    updates_worker::spawn_updates_worker(
        config.clone(),
        holdings_cache.clone(),
        ticket_pool.clone(),
        venue_registry.clone(),
        settle_tx,
        v2cfg.updates_poll_interval_secs,
        lp_shutdown.clone(),
    );
    split_worker::spawn_maintenance_worker(
        config.clone(),
        holdings_cache,
        ticket_pool.clone(),
        split_targets,
        v2cfg.clone(),
        lp_shutdown.clone(),
    );

    if validated.is_empty() {
        if config.rfq_v2_only {
            // rfq_v2_only: no v1 to fall back to — without the atomic stream
            // the agent would quote nothing. Bubble up to the fail-loud arm.
            return Err(anyhow!(
                "RFQ V2 enabled but NO market has a validated AtomicDVP venue — \
                 run `atomic setup` and restart"
            ));
        }
        tracing::error!(
            "RFQ V2 enabled but NO market has a validated AtomicDVP venue — \
             atomic stream not started (v1 continues). Run `atomic setup` and restart."
        );
    } else {
        info!("RFQ V2 enabled for markets: {:?}", validated);
        let config_clone = config.clone();
        let state_clone = state.clone();
        let agent_version = version_info.unwrap_or("unknown").to_string();
        let shutdown_clone = lp_shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = run_lp_atomic_stream(
                config_clone,
                rfq_handler,
                state_clone,
                settle_rx,
                agent_version,
                shutdown_clone,
            )
            .await
            {
                tracing::error!("LP atomic stream failed: {}", e);
            }
        });
    }

    let pool_for_snap = ticket_pool;
    let state_for_snap = state;
    Ok(Arc::new(move || {
        let tickets = pool_for_snap
            .as_ref()
            .map(|p| p.snapshot())
            .unwrap_or_default();
        let pending = state_for_snap.snapshot_pending();
        (tickets, pending)
    }))
}

/// Run a buyer/seller fill loop with background settlement processing
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub async fn run_fill(
    config: BaseConfig,
    direction: fill_loop::FillDirection,
    market: String,
    amount: f64,
    price_limit: Option<f64>,
    min_settlement: f64,
    max_settlement: Option<f64>,
    interval: u64,
    atomic: bool,
    fee_tokens: Vec<String>,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
) -> Result<()> {
    let dir_str = match direction {
        fill_loop::FillDirection::Buy => "buy",
        fill_loop::FillDirection::Sell => "sell",
    };
    info!(
        "Starting {} mode: market={}, amount={}, interval={}s{}{}",
        dir_str,
        market,
        amount,
        interval,
        if atomic { " (atomic RFQ V2)" } else { "" },
        if atomic && !fee_tokens.is_empty() {
            format!(" fee_tokens={fee_tokens:?}")
        } else {
            String::new()
        }
    );
    if !atomic && !fee_tokens.is_empty() {
        warn!("--fee-token only applies to --atomic (RFQ V2) fills — ignored on the v1 path");
    }

    let confirm_lock = agent_logic::confirm::new_confirm_lock();
    let fill_lm = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    // CloudSettlementBackend spawns the ACS / merge / payment-queue workers.
    // Wire Ctrl-C and natural fill-loop completion to its `Shutdown` so those
    // workers exit promptly instead of being cleaned up only when the runtime
    // drops at process exit. (`run_fill_loop` has its own independent Ctrl-C
    // handling — both observe the same signal; `tokio::signal::ctrl_c()` is
    // idempotent across multiple awaiters.)
    let fill_backend_shutdown = Shutdown::new();
    {
        let s = fill_backend_shutdown.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            s.signal();
        });
    }
    let backend = CloudSettlementBackend::new(
        config.clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock.clone(),
        fill_lm,
        fill_backend_shutdown.clone(),
        holdings_cache::HoldingsCache::new(false),
    );

    // CC-only dust merge for the fill path (no ladders here → this cache has no
    // dust thresholds, so the merge worker falls back to legacy total-count
    // consolidation on CC). Preserves the merge that previously ran via the
    // backend constructor.
    if config.merge_threshold.is_some() {
        crate::merge_worker::spawn_merge_worker(
            config.clone(),
            backend.holdings_cache().clone(),
            vec![split_worker::SplitInstrument::cc()],
            fill_backend_shutdown.clone(),
        );
    }

    // The fill loop doesn't use the settlement-machine / payment-queue paths.
    // It settles each accepted quote atomically via MulticallSettler.
    let settler = Arc::new(MulticallSettler {
        config: config.clone(),
        amulet_cache: backend.amulet_cache().clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock: confirm_lock.clone(),
    });

    // RFQ V2 taker driver — shares the backend's holdings cache (its ACS
    // worker populates every instrument, blobs included).
    let atomic_swapper = atomic.then(|| {
        Arc::new(atomic_swap::AtomicSwapper {
            config: config.clone(),
            cache: backend.holdings_cache().clone(),
            // The fill backend's cache is reserve-off, so this is inert; it
            // matters only when a reserve-enabled cache is shared in.
            spend_reserve: false,
            verbose,
            dry_run,
            force,
            confirm,
            confirm_lock,
        })
    });

    let params = fill_loop::FillParams {
        direction,
        market_id: market,
        total_amount: amount,
        price_limit,
        min_settlement,
        max_settlement: max_settlement.unwrap_or(amount),
        interval_secs: interval,
        atomic,
        fee_tokens,
    };

    // Check for saved fill state
    let state_file = PathBuf::from("agent-state.json");
    let saved_fill_state = agent_logic::state::load_state(&state_file)
        .filter(|s| s.party_id == config.party_id)
        .and_then(|s| s.fill_state);

    // Keep `backend` alive so its ACS worker keeps refreshing amulets until return.
    let _backend_guard = backend;
    let result = fill_loop::run_fill_loop(
        config,
        settler,
        params,
        atomic_swapper,
        saved_fill_state,
        Some(state_file),
    )
    .await;
    // Signal backend shutdown on natural completion too — covers paths where
    // the fill loop returns without a Ctrl-C (target filled, error, etc.).
    fill_backend_shutdown.signal();
    result
}

/// Mid-price poller — the ONLY writer to `RfqHandler::mid_prices()`. Feeds V1
/// quoting, RFQ V2 pricing/confirm-time mid checks, and the LIQUIDITY
/// heartbeat's USD-bucket histogram, so it must run whenever the agent is in
/// LP mode — including `rfq_v2_only` mode, where the V1 settlement stream is
/// never opened. `run_cloud_agent` spawns it once per process;
/// `run_lp_settlement_stream` does NOT spawn it.
fn spawn_mid_price_poller(
    price_config: BaseConfig,
    mid_prices: Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, agent_logic::pool_impact::MarketMid>>,
    >,
    price_markets: Vec<String>,
    price_shutdown: Shutdown,
) {
    use agent_logic::client::OrderbookClient;
    use agent_logic::pool_impact::{MarketMid, PoolDepth};

    tokio::spawn(async move {
        let poll_interval = std::time::Duration::from_secs(10);
        // Inner future returns when the poller has nothing left to do (shutdown
        // observed at any sleep / top-of-loop check). Single log site below.
        let result: Result<(), ()> = async {
            if price_shutdown.sleep(std::time::Duration::from_secs(2)).await {
                return Err(());
            }

            // Retry client creation forever (backoff capped at 60s): a one-off
            // connect failure at startup must not permanently kill the poller —
            // without it mid_prices stays empty and the LP never quotes RFQs.
            let mut client = {
                let mut backoff = std::time::Duration::from_secs(5);
                loop {
                    match OrderbookClient::new(&price_config).await {
                        Ok(c) => break c,
                        Err(e) => {
                            tracing::warn!(
                                "Mid-price poller: failed to create client ({}); retrying in {:?}",
                                e, backoff
                            );
                            if price_shutdown.sleep(backoff).await {
                                return Err(());
                            }
                            backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                        }
                    }
                }
            };

            loop {
                if price_shutdown.is_shutting_down() {
                    return Err(());
                }
                for market_id in &price_markets {
                    match client.get_price(market_id).await {
                        Ok(resp) => {
                            let mid = match (resp.bid, resp.ask) {
                                (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
                                _ => resp.last,
                            };
                            if mid > 0.0 && mid.is_finite() {
                                // Any size reference rides the same response
                                // and shares one entry with the mid.
                                let entry = MarketMid {
                                    mid,
                                    pool_depth: resp
                                        .pool_depth
                                        .as_ref()
                                        .and_then(PoolDepth::from_proto),
                                };
                                if mid_prices.write().await.insert(market_id.clone(), entry).is_none()
                                {
                                    tracing::info!(
                                        "Mid-price poller: {} price available ({}); RFQ quoting enabled",
                                        market_id, mid
                                    );
                                }
                            } else {
                                // NO PRICE = NO QUOTES: evict so price_rfq
                                // rejects (TemporarilyUnavailable) instead of
                                // quoting off a stale mid.
                                if mid_prices.write().await.remove(market_id).is_some() {
                                    tracing::warn!(
                                        "Mid-price poller: {} returned non-positive mid ({}); \
                                         evicting — RFQ quoting paused",
                                        market_id, mid
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            // NO PRICE = NO QUOTES. Evict rather than quote
                            // off the last-known mid; warn on the transition.
                            if mid_prices.write().await.remove(market_id).is_some() {
                                tracing::warn!(
                                    "Mid-price poller: {} has NO price ({}); evicting stale mid — \
                                     RFQ quoting paused until the price returns",
                                    market_id, e
                                );
                            } else {
                                tracing::debug!("Mid-price poller: {} error: {}", market_id, e);
                            }
                        }
                    }
                }
                if price_shutdown.sleep(poll_interval).await {
                    return Err(());
                }
            }
        }
        .await;
        if result.is_err() {
            info!("Mid-price poller shutting down");
        }
    });
}

/// Run the LP settlement stream (bidirectional gRPC for RFQ handling).
/// Never spawned when `rfq_v2_only = true` — without the handshake's
/// `liquidity_provider_name` registration the server routes no V1 RFQs to
/// this party and it drops out of `GetConnectedLiquidityProviders`.
pub async fn run_lp_settlement_stream(
    config: BaseConfig,
    rfq_handler: Arc<rfq_handler::RfqHandler>,
    shutdown: Shutdown,
) -> Result<()> {
    use orderbook_proto::settlement::{
        CantonNodeAuth, CantonToServerMessage, Heartbeat, SettlementHandshake,
        canton_to_server_message::Message as CantonMessage,
        server_to_canton_message::Message as ServerMessage,
        settlement_service_client::SettlementServiceClient,
    };
    use rfq_handler::RfqResponse;
    use tokio_stream::StreamExt;

    let lp_config = config
        .liquidity_provider
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No LP config"))?;

    loop {
        if shutdown.is_shutting_down() {
            info!("LP stream shutting down, not reconnecting");
            return Ok(());
        }

        info!(
            "Connecting LP settlement stream to {}",
            config.orderbook_grpc_url
        );

        let channel = match create_raw_channel(&config.orderbook_grpc_url).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to connect: {}, retrying in 5s", e);
                if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                    return Ok(());
                }
                continue;
            }
        };

        let mut client =
            SettlementServiceClient::new(channel).max_decoding_message_size(16 * 1024 * 1024);

        // Create the outbound channel
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel::<CantonToServerMessage>(64);
        let outbound_stream = tokio_stream::wrappers::ReceiverStream::new(outbound_rx);

        let auth_header = agent_logic::auth::generate_jwt(
            &config.party_id,
            &config.role,
            &config.private_key.expose(),
            config.token_ttl_secs,
            Some(&config.node_name),
        )
        .map_err(|e| anyhow!("{}", e))
        .and_then(|jwt| {
            format!("Bearer {}", jwt)
                .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                .map_err(|e| anyhow!("{}", e))
        });
        let auth_header = match auth_header {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(
                    "LP settlement stream: failed to build auth token: {}, retrying in 5s",
                    e
                );
                if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                    return Ok(());
                }
                continue;
            }
        };
        let mut open_request = tonic::Request::new(outbound_stream);
        open_request
            .metadata_mut()
            .insert("authorization", auth_header);

        // Open bidirectional stream
        let response = match client.settlement_stream(open_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to open settlement stream: {}, retrying in 5s", e);
                if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                    return Ok(());
                }
                continue;
            }
        };

        let mut inbound = response.into_inner();

        // Send handshake with LP name
        let handshake = CantonToServerMessage {
            session_id: String::new(),
            sequence_number: 0,
            message: Some(CantonMessage::Handshake(SettlementHandshake {
                auth: Some(CantonNodeAuth {
                    party_id: config.party_id.clone(),
                    jwt_token: String::new(),
                    node_instance: config.node_name.clone(),
                    connected_at: Some(prost_types::Timestamp {
                        seconds: chrono::Utc::now().timestamp(),
                        nanos: 0,
                    }),
                }),
                party_ids: vec![config.party_id.clone()],
                user_services: vec![],
                operator_party: config.settlement_operator.clone(),
                capabilities: None,
                liquidity_provider_name: Some(lp_config.name.clone()),
            })),
            sent_at: Some(prost_types::Timestamp {
                seconds: chrono::Utc::now().timestamp(),
                nanos: 0,
            }),
        };

        if outbound_tx.send(handshake).await.is_err() {
            tracing::error!("Failed to send handshake, retrying");
            if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                return Ok(());
            }
            continue;
        }

        info!("LP settlement stream connected, listening for RFQ requests");

        // Send a heartbeat every 30s. If the outbound send fails, the stream's
        // write half is closed — reconnect. Tonic doesn't surface silent
        // half-closes on the read side, so we can't rely on inbound activity
        // alone; the heartbeat send-failure is what detects a dead stream.
        const HEARTBEAT_INTERVAL_SECS: u64 = 30;
        let mut heartbeat_interval =
            tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick so we don't send a heartbeat 0s after connect.
        heartbeat_interval.tick().await;
        let mut client_seq: u64 = 0;

        loop {
            tokio::select! {
                biased;
                // Observe shutdown the moment select is idle. This does NOT
                // cancel an in-flight RFQ-handling body — once `inbound.next()`
                // has fired and we're processing a message, the body runs to
                // completion. The shutdown arm only fires when waiting.
                _ = shutdown.wait() => {
                    info!("LP settlement stream observed shutdown, breaking inner loop");
                    break;
                }
                _ = heartbeat_interval.tick() => {
                    client_seq += 1;
                    let now = prost_types::Timestamp {
                        seconds: chrono::Utc::now().timestamp(),
                        nanos: 0,
                    };
                    let hb = CantonToServerMessage {
                        session_id: String::new(),
                        sequence_number: client_seq,
                        message: Some(CantonMessage::Heartbeat(Heartbeat {
                            session_id: String::new(),
                            sequence_number: client_seq,
                            timestamp: Some(now.clone()),
                        })),
                        sent_at: Some(now),
                    };
                    if outbound_tx.send(hb).await.is_err() {
                        tracing::warn!("LP settlement stream send-failed on heartbeat, reconnecting");
                        break;
                    }
                }
                msg_result = inbound.next() => {
                    let msg = match msg_result {
                        None => {
                            tracing::warn!("LP settlement stream ended (None), reconnecting");
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::error!("LP settlement stream error: {}", e);
                            break;
                        }
                        Some(Ok(m)) => m,
                    };

                    match msg.message {
                        Some(ServerMessage::HandshakeAck(ack)) => {
                            info!("LP handshake acknowledged: accepted={}", ack.accepted);
                        }
                        Some(ServerMessage::RfqRequest(request)) => {
                            if shutdown.is_shutting_down() {
                                info!("Ignoring RFQ {} - shutting down", request.rfq_id);
                                break;
                            }
                            info!(
                                "Received RFQ request: rfq_id={}, market={}, direction={}, qty={}",
                                request.rfq_id, request.market_id, request.direction, request.quantity
                            );

                            let response = rfq_handler.handle_rfq_request(request).await;

                            let response_msg = match response {
                                RfqResponse::Quote(quote) => CantonToServerMessage {
                                    session_id: String::new(),
                                    sequence_number: 0,
                                    message: Some(CantonMessage::RfqQuote(quote)),
                                    sent_at: Some(prost_types::Timestamp {
                                        seconds: chrono::Utc::now().timestamp(),
                                        nanos: 0,
                                    }),
                                },
                                RfqResponse::Reject(reject) => CantonToServerMessage {
                                    session_id: String::new(),
                                    sequence_number: 0,
                                    message: Some(CantonMessage::RfqReject(reject)),
                                    sent_at: Some(prost_types::Timestamp {
                                        seconds: chrono::Utc::now().timestamp(),
                                        nanos: 0,
                                    }),
                                },
                            };

                            if outbound_tx.send(response_msg).await.is_err() {
                                tracing::error!("Failed to send RFQ response, stream may be closed");
                                break;
                            }
                        }
                        Some(ServerMessage::Heartbeat(_)) => {
                            // Server-side keepalive — no action needed.
                        }
                        other => {
                            tracing::debug!("LP stream received unhandled message: {:?}", other.map(|_| "..."));
                        }
                    }
                }
            }
        }

        if shutdown.is_shutting_down() {
            info!("LP stream shutting down after disconnect");
            return Ok(());
        }
        tracing::warn!("LP settlement stream disconnected, reconnecting in 5s");
        if shutdown.sleep(std::time::Duration::from_secs(5)).await {
            return Ok(());
        }
    }
}

/// Run the RFQ V2 atomic stream (design §5.5): a second bidi stream parallel
/// to the v1 settlement stream. Phase 1 (AtomicRfqRequest) prices through the
/// SHARED v1 pipeline with an advisory availability check (no reserve); phase
/// 2 (RfqConfirmRequest) commits the LiquidityManager funds, hard-reserves
/// holdings, signs, and returns the disclosure envelope.
pub async fn run_lp_atomic_stream(
    config: BaseConfig,
    rfq_handler: Arc<rfq_handler::RfqHandler>,
    state: Arc<rfq_v2::RfqV2State>,
    mut settle_rx: tokio::sync::mpsc::UnboundedReceiver<rfq_v2::SettleObserved>,
    agent_version: String,
    shutdown: Shutdown,
) -> Result<()> {
    use orderbook_proto::rfqv2::{
        AtomicHandshake, AtomicHeartbeat, AtomicLpToServer, AtomicRfqReject,
        atomic_lp_to_server::Message as LpMessage,
        atomic_rfq_service_client::AtomicRfqServiceClient,
        atomic_server_to_lp::Message as ServerMessage,
    };
    use tokio_stream::StreamExt;

    let lp_name = state.lp_name().to_string();

    // Settle-observation consumer: lives across stream reconnects so watcher
    // events are handled even while the stream is down.
    {
        let state_clone = state.clone();
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_clone.wait() => return,
                    obs = settle_rx.recv() => {
                        match obs {
                            Some(obs) => {
                                state_clone.handle_settle_observed(&obs.quote_id, &obs.update_id).await;
                            }
                            None => return, // watcher gone
                        }
                    }
                }
            }
        });
    }

    fn now_ts() -> prost_types::Timestamp {
        prost_types::Timestamp {
            seconds: chrono::Utc::now().timestamp(),
            nanos: 0,
        }
    }
    fn wrap(seq: u64, message: LpMessage) -> AtomicLpToServer {
        AtomicLpToServer {
            session_id: String::new(),
            sequence_number: seq,
            message: Some(message),
            sent_at: Some(now_ts()),
        }
    }

    loop {
        if shutdown.is_shutting_down() {
            info!("LP atomic stream shutting down, not reconnecting");
            return Ok(());
        }

        info!(
            "Connecting LP atomic stream to {}",
            config.orderbook_grpc_url
        );

        let channel = match create_raw_channel(&config.orderbook_grpc_url).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Atomic stream: failed to connect: {}, retrying in 5s", e);
                if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                    return Ok(());
                }
                continue;
            }
        };

        let mut client =
            AtomicRfqServiceClient::new(channel).max_decoding_message_size(16 * 1024 * 1024);

        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel::<AtomicLpToServer>(64);
        let outbound_stream = tokio_stream::wrappers::ReceiverStream::new(outbound_rx);

        // The V2 stream requires a Bearer JWT at open (no CantonNodeAuth
        // fallback, unlike the v1 settlement stream). Fresh per reconnect.
        let auth_header = agent_logic::auth::generate_jwt(
            &config.party_id,
            &config.role,
            &config.private_key.expose(),
            config.token_ttl_secs,
            Some(&config.node_name),
        )
        .map_err(|e| anyhow!("{}", e))
        .and_then(|jwt| {
            format!("Bearer {}", jwt)
                .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                .map_err(|e| anyhow!("{}", e))
        });
        let auth_header = match auth_header {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(
                    "Atomic stream: failed to build auth token: {}, retrying in 5s",
                    e
                );
                if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                    return Ok(());
                }
                continue;
            }
        };
        let mut open_request = tonic::Request::new(outbound_stream);
        open_request
            .metadata_mut()
            .insert("authorization", auth_header);

        let response = match client.atomic_rfq_stream(open_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to open atomic RFQ stream: {}, retrying in 5s", e);
                if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                    return Ok(());
                }
                continue;
            }
        };

        let mut inbound = response.into_inner();

        let handshake = wrap(
            0,
            LpMessage::Handshake(AtomicHandshake {
                party_ids: vec![config.party_id.clone()],
                lp_name: lp_name.clone(),
                agent_version: agent_version.clone(),
                market_ids: state.validated_market_ids(),
            }),
        );
        if outbound_tx.send(handshake).await.is_err() {
            tracing::error!("Atomic stream: failed to send handshake, retrying");
            if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                return Ok(());
            }
            continue;
        }

        info!("LP atomic stream connected, listening for atomic RFQ requests");

        const HEARTBEAT_INTERVAL_SECS: u64 = 30;
        let mut heartbeat_interval =
            tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat_interval.tick().await; // skip the immediate first tick
        let mut sweep_interval = tokio::time::interval(std::time::Duration::from_secs(10));
        sweep_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut client_seq: u64 = 0;

        loop {
            tokio::select! {
                biased;
                _ = shutdown.wait() => {
                    info!("LP atomic stream observed shutdown, breaking inner loop");
                    break;
                }
                _ = heartbeat_interval.tick() => {
                    client_seq += 1;
                    let hb = wrap(client_seq, LpMessage::Heartbeat(AtomicHeartbeat { at: Some(now_ts()) }));
                    if outbound_tx.send(hb).await.is_err() {
                        tracing::warn!("LP atomic stream send-failed on heartbeat, reconnecting");
                        break;
                    }
                }
                _ = sweep_interval.tick() => {
                    // Releases expired confirm-time LM commitments / hard
                    // reserves / ticket assignments (traceability row 9)
                    state.sweep(std::time::Instant::now()).await;
                }
                msg_result = inbound.next() => {
                    let msg = match msg_result {
                        None => {
                            tracing::warn!("LP atomic stream ended (None), reconnecting");
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::error!("LP atomic stream error: {}", e);
                            break;
                        }
                        Some(Ok(m)) => m,
                    };

                    match msg.message {
                        Some(ServerMessage::HandshakeAck(ack)) => {
                            info!("LP atomic handshake acknowledged: success={} session={}", ack.success, ack.session_id);
                        }
                        Some(ServerMessage::Heartbeat(_)) => {
                            // Server-side keepalive — no action needed.
                        }
                        Some(ServerMessage::RfqRequest(request)) => {
                            if shutdown.is_shutting_down() {
                                info!("Ignoring atomic RFQ {} - shutting down", request.rfq_id);
                                break;
                            }
                            // Venue identity for [[venue_overrides]] pricing:
                            // explicit venue_name, falling back to the VA2
                            // attribution prefix for servers predating it.
                            let rfq_venue = request
                                .venue_name
                                .as_deref()
                                .filter(|s| !s.is_empty())
                                .or(request.quote_id_prefix.as_deref().filter(|s| !s.is_empty()));
                            let rfq_venue_branch =
                                request.venue_branch.as_deref().filter(|s| !s.is_empty());
                            // Requesting party id; keys the per-counterparty
                            // accumulator. Absent ⇒ no per-party term.
                            let rfq_user_party =
                                request.user_party.as_deref().filter(|s| !s.is_empty());
                            info!(
                                "Received atomic RFQ: rfq_id={}, market={}, direction={}, qty={}, venue={}{}",
                                request.rfq_id, request.market_id, request.direction, request.quantity,
                                rfq_venue.unwrap_or("-"),
                                rfq_venue_branch.map(|b| format!("/{b}")).unwrap_or_default()
                            );

                            let reject = |reason: String, min: String, max: String| {
                                LpMessage::Reject(AtomicRfqReject {
                                    rfq_id: request.rfq_id.clone(),
                                    market_id: request.market_id.clone(),
                                    reason,
                                    lp_party_id: config.party_id.clone(),
                                    min_quantity: min,
                                    max_quantity: max,
                                })
                            };

                            // V2 direction is a string; the shared pricing fn
                            // takes the v1 i32 enum (1=BUY user buys, 2=SELL)
                            let direction = match request.direction.to_ascii_lowercase().as_str() {
                                "buy" => 1,
                                "sell" => 2,
                                _ => 0,
                            };

                            let message = if direction == 0 {
                                reject(format!("invalid direction '{}'", request.direction), String::new(), String::new())
                            } else if !state.quotable(&request.market_id) {
                                reject("market not available for atomic RFQ".to_string(), String::new(), String::new())
                            } else {
                                match rfq_handler
                                    .price_rfq(
                                        &request.rfq_id,
                                        &request.market_id,
                                        direction,
                                        &request.quantity,
                                        request.quote_quantity.as_deref().unwrap_or(""),
                                        // V2: no min-notional floor — the user
                                        // pays every fee (3x dust surcharge
                                        // server-side); the LP pays none.
                                        false,
                                        rfq_venue,
                                        rfq_venue_branch,
                                        rfq_user_party,
                                    )
                                    .await
                                {
                                    Err(r) => reject(
                                        r.reason_detail.unwrap_or_else(|| format!("{:?}", r.reason)),
                                        r.min_quantity.unwrap_or_default(),
                                        r.max_quantity.unwrap_or_default(),
                                    ),
                                    Ok(priced) => {
                                        // Venue-attribution (VA2): when the server supplies a
                                        // prefix, the quote id becomes "<venue>-<uuidv7>" so the
                                        // venue slug rides the signed canonical message
                                        // (quote_nonce) onto the chain. The server DROPS quotes
                                        // that fail to echo the expected prefix.
                                        let quote_id = match request.quote_id_prefix.as_deref() {
                                            Some(prefix) if !prefix.is_empty() => {
                                                format!("{prefix}-{}", uuid::Uuid::now_v7())
                                            }
                                            _ => uuid::Uuid::now_v7().to_string(),
                                        };
                                        let side = if direction == 1 {
                                            atomic_quote::QuoteSide::Buy
                                        } else {
                                            atomic_quote::QuoteSide::Sell
                                        };
                                        match state
                                            .register_indicative(
                                                &quote_id,
                                                &request.market_id,
                                                side,
                                                &priced,
                                                request.settlement_fee.clone(),
                                                rfq_user_party,
                                            )
                                            .await
                                        {
                                            Err(e) => reject(e, String::new(), String::new()),
                                            Ok(()) => {
                                                let now = chrono::Utc::now();
                                                let valid_until = now
                                                    + chrono::Duration::seconds(priced.valid_for_secs as i64);
                                                LpMessage::Quote(orderbook_proto::rfqv2::AtomicRfqQuote {
                                                    rfq_id: request.rfq_id.clone(),
                                                    quote_id,
                                                    market_id: request.market_id.clone(),
                                                    direction: request.direction.clone(),
                                                    price: priced.price_str.clone(),
                                                    quantity: priced.quantity_str.clone(),
                                                    quote_quantity: priced.quote_quantity_str.clone(),
                                                    valid_for_secs: priced.valid_for_secs,
                                                    valid_until: Some(prost_types::Timestamp {
                                                        seconds: valid_until.timestamp(),
                                                        nanos: 0,
                                                    }),
                                                    lp_party_id: config.party_id.clone(),
                                                    lp_name: lp_name.clone(),
                                                    quoted_at: Some(now_ts()),
                                                    // echo of the authoritative fee — the relay
                                                    // drops the quote on any mismatch (design §14 D19)
                                                    settlement_fee: request.settlement_fee.clone(),
                                                })
                                            }
                                        }
                                    }
                                }
                            };

                            client_seq += 1;
                            if outbound_tx.send(wrap(client_seq, message)).await.is_err() {
                                tracing::error!("Failed to send atomic RFQ response, stream may be closed");
                                break;
                            }
                        }
                        Some(ServerMessage::ConfirmRequest(req)) => {
                            info!(
                                "Received atomic confirm: rfq_id={}, quote_id={}, user={}",
                                req.rfq_id, req.quote_id, req.user_party
                            );
                            let message = match state.handle_confirm(req).await {
                                Ok(envelope) => LpMessage::Envelope(envelope),
                                Err(reject) => LpMessage::ConfirmReject(reject),
                            };
                            client_seq += 1;
                            if outbound_tx.send(wrap(client_seq, message)).await.is_err() {
                                tracing::error!("Failed to send atomic confirm response, stream may be closed");
                                break;
                            }
                        }
                        None => {
                            tracing::debug!("LP atomic stream received empty message");
                        }
                    }
                }
            }
        }

        if shutdown.is_shutting_down() {
            info!("LP atomic stream shutting down after disconnect");
            return Ok(());
        }
        tracing::warn!("LP atomic stream disconnected, reconnecting in 5s");
        if shutdown.sleep(std::time::Duration::from_secs(5)).await {
            return Ok(());
        }
    }
}

// ============================================================================
// Info commands
// ============================================================================

pub fn prost_struct_to_json(s: &prost_types::Struct) -> serde_json::Value {
    let map = s
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), prost_value_to_json(v)))
        .collect();
    serde_json::Value::Object(map)
}

pub fn prost_value_to_json(v: &prost_types::Value) -> serde_json::Value {
    match &v.kind {
        Some(prost_types::value::Kind::NullValue(_)) => serde_json::Value::Null,
        Some(prost_types::value::Kind::NumberValue(n)) => serde_json::Value::Number(
            serde_json::Number::from_f64(*n).unwrap_or_else(|| serde_json::Number::from(0)),
        ),
        Some(prost_types::value::Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(prost_types::value::Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        Some(prost_types::value::Kind::StructValue(s)) => prost_struct_to_json(s),
        Some(prost_types::value::Kind::ListValue(l)) => {
            serde_json::Value::Array(l.values.iter().map(prost_value_to_json).collect())
        }
        None => serde_json::Value::Null,
    }
}

pub fn format_value(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        _ => val.to_string(),
    }
}

pub fn print_json_value(value: &serde_json::Value, indent: usize) {
    let prefix = "   ".repeat(indent);
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                match val {
                    serde_json::Value::Object(_) => {
                        println!("{}{}:", prefix, key);
                        print_json_value(val, indent + 1);
                    }
                    serde_json::Value::Array(arr) if !arr.is_empty() && arr[0].is_object() => {
                        println!("{}{}:", prefix, key);
                        for (i, item) in arr.iter().enumerate() {
                            println!("{}   [{}]:", prefix, i);
                            print_json_value(item, indent + 2);
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        if arr.is_empty() {
                            println!("{}{}: []", prefix, key);
                        } else {
                            let items: Vec<String> = arr.iter().map(format_value).collect();
                            println!("{}{}: [{}]", prefix, key, items.join(", "));
                        }
                    }
                    _ => {
                        println!("{}{}: {}", prefix, key, format_value(val));
                    }
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                println!("{}[{}]:", prefix, i);
                print_json_value(item, indent + 1);
            }
        }
        _ => println!("{}{}", prefix, format_value(value)),
    }
}

pub fn print_info_list_table(contracts: &[orderbook_proto::ledger::ActiveContractInfo]) {
    if contracts.is_empty() {
        println!("No contracts found.");
        return;
    }

    println!("Found {} contracts:\n", contracts.len());

    for (i, contract) in contracts.iter().enumerate() {
        println!("Contract #{}:", i + 1);
        println!("   Contract ID: {}", contract.contract_id);
        println!("   Template ID: {}", contract.template_id);
        println!("   Payload:");
        if let Some(args) = &contract.create_arguments {
            let json = prost_struct_to_json(args);
            print_json_value(&json, 2);
        }
        println!();
    }
}

pub fn print_info_list_count(contracts: &[orderbook_proto::ledger::ActiveContractInfo]) {
    let mut template_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for contract in contracts {
        *template_counts
            .entry(contract.template_id.clone())
            .or_insert(0) += 1;
    }

    println!("{:>6}  {}", "Count", "Template");
    println!("{}", "-".repeat(120));

    let mut sorted: Vec<_> = template_counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));

    for (template, count) in sorted {
        println!("{:>6}  {}", count, template);
    }

    println!("{}", "-".repeat(120));
    println!("{:>6}  {}", contracts.len(), "Total");
}

pub async fn run_info(config: BaseConfig, command: InfoCommands) -> Result<()> {
    if let InfoCommands::Party = &command {
        println!("Party ID: {}", config.party_id);
        println!("Public key: {}", config.public_key_hex);
        println!("Node name: {}", config.node_name);
        return Ok(());
    }

    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        InfoCommands::List { template, count } => {
            let filters: Vec<String> = template.into_iter().collect();
            let contracts = client.get_active_contracts(&filters).await?;
            if count {
                print_info_list_count(&contracts);
            } else {
                print_info_list_table(&contracts);
            }
        }
        InfoCommands::Party => unreachable!(),
        InfoCommands::Balance => {
            let balances = client.get_balances().await?;
            if balances.is_empty() {
                println!("No balances found");
            } else {
                for b in &balances {
                    let name = if b.is_canton_coin {
                        "CC".to_string()
                    } else {
                        b.instrument_id.clone()
                    };
                    println!(
                        "{}: {} (locked: {}, unlocked: {})",
                        name, b.total_amount, b.locked_amount, b.unlocked_amount
                    );
                }
            }
        }
        InfoCommands::PrepaidTraffic => {
            let pt = client.get_prepaid_traffic_balance().await?;
            println!("\n=== Prepaid Traffic Balance ===\n");
            println!("Balance:           {} CC", pt.balance_cc);
            println!("Credit limit:      {} CC", pt.credit_limit_cc);
            println!("Available:         {} CC", pt.available_cc);
            println!("Total credited:    {} CC", pt.total_credited_cc);
            println!("Total debited:     {} CC", pt.total_debited_cc);
        }
        InfoCommands::Network => {
            print_network_info(&client.get_dso_rates().await?);
        }
    }

    Ok(())
}

// ============================================================================
// Preapproval commands
// ============================================================================

pub async fn run_preapproval(
    config: BaseConfig,
    command: PreapprovalCommands,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        PreapprovalCommands::Request {
            instrument_admin,
            replace,
        } => {
            // An unscoped preapproval already covers every instrument of this
            // admin; checked before resolving the operator so an unknown admin
            // cannot mask "nothing to do".
            if !replace
                && client.get_preapprovals().await?.iter().any(|p| {
                    p.receiver == config.party_id
                        && p.instrument_admin == instrument_admin
                        && p.instrument_allowances.is_empty()
                })
            {
                println!(
                    "Preapproval for admin {} already exists; nothing to do (pass --replace to create another).",
                    instrument_admin
                );
                return Ok(());
            }
            // Resolve the operator party from the faucet instrument list,
            // matching the requested registry/admin.
            let faucet_instruments = client
                .list_faucet_instruments()
                .await
                .context("Failed to fetch faucet instruments from payments-rpc")?;
            let operator = operator_for_registry(&faucet_instruments, &instrument_admin)?;
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Create CIP-56 Preapproval",
                    &format!(
                        "party: {}, admin: {}, operator: {}",
                        config.party_id, instrument_admin, operator
                    ),
                )
                .await?;
            }
            let expectation = OperationExpectation::RequestPreapproval {
                party: config.party_id.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestPreapproval as i32,
                        params: Some(Params::RequestPreapproval(RequestPreapprovalParams {
                            instrument_admin,
                            instrument_allowances: vec![],
                            operator,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            println!(
                "CIP-56 preapproval created, update id: {}",
                result.update_id
            );
        }
        PreapprovalCommands::Fetch => {
            let preapprovals = client.get_preapprovals().await?;
            if preapprovals.is_empty() {
                println!("No preapprovals found");
            } else {
                for p in &preapprovals {
                    let allowances = if p.instrument_allowances.is_empty() {
                        "all".to_string()
                    } else {
                        p.instrument_allowances
                            .iter()
                            .map(|a| a.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    println!(
                        "{}: operator={} receiver={} admin={} instruments=[{}]",
                        p.contract_id, p.operator, p.receiver, p.instrument_admin, allowances
                    );
                }
            }
        }
        PreapprovalCommands::Check | PreapprovalCommands::Sync => {
            let create = matches!(command, PreapprovalCommands::Sync);
            let advertised = client
                .list_faucet_instruments()
                .await
                .context("Failed to fetch preapproval targets")?;
            let held = client.get_preapprovals().await?;
            let (targets, warnings) =
                preapproval_targets(&held, &advertised, &config.dso_party, &config.party_id);

            // The state column is driven by `targets` so the table and the
            // summary below can never disagree.
            let needed: std::collections::HashSet<&str> =
                targets.iter().map(|t| t.admin.as_str()).collect();
            println!("Preapproval targets advertised by the server:");
            for inst in &advertised {
                let mine = |p: &&PreapprovalInfo| {
                    p.receiver == config.party_id && p.instrument_admin == inst.registry
                };
                let scoped = held
                    .iter()
                    .filter(mine)
                    .any(|p| !p.instrument_allowances.is_empty());
                let state = if inst.token_name == "Amulet" || inst.registry == config.dso_party {
                    "CC — Splice path, not checked here".to_string()
                } else if needed.contains(inst.registry.as_str()) {
                    if scoped {
                        "MISSING (only a scoped preapproval is held)".to_string()
                    } else {
                        "MISSING".to_string()
                    }
                } else {
                    "held".to_string()
                };
                println!(
                    "  {:<10} admin={} operator={} -> {}",
                    inst.token_name, inst.registry, inst.operator, state
                );
            }
            let advertised_admins: std::collections::HashSet<&str> =
                advertised.iter().map(|i| i.registry.as_str()).collect();
            let stale: Vec<&PreapprovalInfo> = held
                .iter()
                .filter(|p| {
                    p.receiver == config.party_id
                        && !advertised_admins.contains(p.instrument_admin.as_str())
                })
                .collect();
            if !stale.is_empty() {
                println!("Held for admins the server does not advertise:");
                for p in stale {
                    println!("  admin={} operator={}", p.instrument_admin, p.operator);
                }
            }
            for w in &warnings {
                println!("Warning: {}", w);
            }

            if targets.is_empty() {
                println!("\nNothing to create.");
            } else if !create {
                println!(
                    "\n{} registry/registries need a CIP-56 preapproval; run `preapproval sync`.",
                    targets.len()
                );
            } else {
                if confirm && !dry_run {
                    let lock = agent_logic::confirm::new_confirm_lock();
                    agent_logic::confirm::confirm_transaction(
                        &lock,
                        "Create CIP-56 Preapprovals",
                        &format!(
                            "party: {}, admins: {}",
                            config.party_id,
                            targets
                                .iter()
                                .map(|t| t.admin.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                    .await?;
                }
                let mut failures: Vec<String> = Vec::new();
                for t in &targets {
                    println!(
                        "Creating CIP-56 preapproval for admin={} (instruments: {})...",
                        t.admin,
                        t.labels.join(", ")
                    );
                    let expectation = OperationExpectation::RequestPreapproval {
                        party: config.party_id.clone(),
                    };
                    match client
                        .submit_transaction(
                            PrepareTransactionRequest {
                                operation: TransactionOperation::RequestPreapproval as i32,
                                params: Some(Params::RequestPreapproval(
                                    RequestPreapprovalParams {
                                        instrument_admin: t.admin.clone(),
                                        instrument_allowances: vec![],
                                        operator: t.operator.clone(),
                                    },
                                )),
                                request_signature: None,
                            },
                            &expectation,
                            verbose,
                            dry_run,
                            force,
                        )
                        .await
                    {
                        Ok(r) if r.success => println!("Created (update {}).", r.update_id),
                        Ok(_) => println!("Dry run — not submitted."),
                        Err(e) => {
                            println!("Warning: admin={} failed: {:#}", t.admin, e);
                            failures.push(t.admin.clone());
                        }
                    }
                }
                if !failures.is_empty() {
                    anyhow::bail!("{} preapproval(s) failed: {}", failures.len(), failures.join(", "));
                }
            }
        }
    }

    Ok(())
}

// ============================================================================
// Subscription payment commands
// ============================================================================

pub async fn run_subscription(
    config: BaseConfig,
    command: SubscriptionCommands,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        SubscriptionCommands::RequestPrepaid {
            amount,
            limit,
            locked_amount,
            lock_days,
            description,
            reference,
        } => {
            let app = config.settlement_operator.clone();
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Request subscription prepaid",
                    &format!("app: {}, amount: {}/day, limit: {}", app, amount, limit),
                )
                .await?;
            }
            let expectation = OperationExpectation::RequestRecurringPrepaid {
                party: config.party_id.clone(),
                app_party: app.clone(),
                amount: amount.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestRecurringPrepaid as i32,
                        params: Some(Params::RequestRecurringPrepaid(
                            RequestRecurringPrepaidParams {
                                app_party: app,
                                amount,
                                limit,
                                locked_amount,
                                lock_days,
                                description,
                                reference,
                            },
                        )),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            println!("Recurring prepaid requested: {}", result.update_id);
        }
        SubscriptionCommands::RequestPayasyougo {
            app,
            amount,
            description,
            reference,
        } => {
            let app = app.unwrap_or_else(|| config.settlement_operator.clone());
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Request subscription pay-as-you-go",
                    &format!("app: {}, amount: {}", app, amount),
                )
                .await?;
            }
            let expectation = OperationExpectation::RequestRecurringPayasyougo {
                party: config.party_id.clone(),
                app_party: app.clone(),
                amount: amount.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestRecurringPayasyougo as i32,
                        params: Some(Params::RequestRecurringPayasyougo(
                            RequestRecurringPayasyougoParams {
                                app_party: app,
                                amount,
                                description,
                                reference,
                            },
                        )),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            println!("Recurring pay-as-you-go requested: {}", result.update_id);
        }
    }

    Ok(())
}

// ============================================================================
// Transfer commands
// ============================================================================

pub async fn run_transfer(
    config: BaseConfig,
    command: TransferCommands,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        TransferCommands::SendCc {
            receiver,
            amount,
            description,
            memo_uuid,
        } => {
            let description = if memo_uuid {
                Some(uuid::Uuid::now_v7().to_string())
            } else {
                description
            };
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Transfer CC",
                    &format!("receiver: {}, amount: {}", receiver, amount),
                )
                .await?;
            }
            let command_id = format!("cli-cc-{}", uuid::Uuid::now_v7());
            let expectation = OperationExpectation::TransferCc {
                sender_party: config.party_id.clone(),
                receiver_party: receiver.clone(),
                amount: amount.clone(),
                command_id: command_id.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::TransferCc as i32,
                        params: Some(Params::TransferCc(TransferCcParams {
                            receiver_party: receiver,
                            amount,
                            description,
                            command_id,
                            settlement_proposal_id: None,
                            amulet_cids: vec![],
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            if !result.update_id.is_empty() {
                println!("CC transfer sent: {}", result.update_id);
                report_transfer_outcome(&result.created_contracts);
            }
        }
        TransferCommands::SendCip56 {
            receiver,
            instrument_id,
            instrument_admin,
            amount,
            reference,
            count,
        } => {
            parse_payment_amount(&amount, &receiver, 1)?;
            // The ledger only knows on-chain wire ids; operators think in names.
            let resolved = config.resolve_instrument_or_wire(&instrument_id);
            match &resolved.wire_id {
                Some(wire) if *wire == instrument_id => println!("Instrument: {}", instrument_id),
                Some(wire) => println!("Instrument: {} (wire id {})", instrument_id, wire),
                None => eprintln!(
                    "warning: '{}' is not in the instrument registry — sending it verbatim as the wire id",
                    instrument_id
                ),
            }
            let instrument_admin = match instrument_admin {
                Some(admin) => admin,
                None => match &resolved.registry {
                    Some(registry) => {
                        println!("Registry:   {} (from instrument registry)", registry);
                        registry.clone()
                    }
                    None => {
                        return Err(anyhow::anyhow!(
                            "no registry recorded for instrument '{}' — pass --instrument-admin",
                            instrument_id
                        ))
                    }
                },
            };
            let wire_instrument_id = resolved.wire_id.clone().unwrap_or_else(|| instrument_id.clone());

            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                let label = if wire_instrument_id == instrument_id {
                    format!("Transfer CIP-56 ({})", instrument_id)
                } else {
                    format!("Transfer CIP-56 ({} / wire {})", instrument_id, wire_instrument_id)
                };
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    &label,
                    &format!("receiver: {}, amount: {}", receiver, amount),
                )
                .await?;
            }
            let expectation = OperationExpectation::TransferCip56 {
                sender_party: config.party_id.clone(),
                receiver_party: receiver.clone(),
                instrument_id: wire_instrument_id.clone(),
                instrument_admin: instrument_admin.clone(),
                amount: amount.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::TransferCip56 as i32,
                        params: Some(Params::TransferCip56(TransferCip56Params {
                            instrument_id: wire_instrument_id.clone(),
                            instrument_admin,
                            receiver_party: receiver,
                            amount,
                            reference,
                            // CLI send: auto-select inputs (empty) — the merge
                            // worker is the only caller that passes explicit cids.
                            input_holding_cids: Vec::new(),
                            max_input_holdings: count,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            if !result.update_id.is_empty() {
                println!(
                    "CIP-56 transfer sent ({}): {}",
                    instrument_id, result.update_id
                );
                report_transfer_outcome(&result.created_contracts);
            }
        }
        TransferCommands::AcceptCip56 { contract_id } => {
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Accept CIP-56 transfer",
                    &format!("contract: {}", contract_id),
                )
                .await?;
            }
            let expectation = OperationExpectation::AcceptCip56 {
                receiver_party: config.party_id.clone(),
                contract_id: contract_id.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::AcceptCip56 as i32,
                        params: Some(Params::AcceptCip56(AcceptCip56Params {
                            contract_id: contract_id.clone(),
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            if result.update_id.is_empty() {
                return Ok(());
            }
            println!(
                "CIP-56 transfer accepted ({}): {}",
                contract_id, result.update_id
            );
        }
        TransferCommands::SplitCc {
            output_amounts,
            amulet_cids,
        } => {
            if output_amounts.is_empty() {
                return Err(anyhow::anyhow!("--output-amounts must not be empty"));
            }
            if amulet_cids.is_empty() {
                return Err(anyhow::anyhow!("--amulet-cids must not be empty"));
            }
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Split CC",
                    &format!(
                        "outputs: [{}], inputs: {} amulets",
                        output_amounts.join(", "),
                        amulet_cids.len()
                    ),
                )
                .await?;
            }
            let expectation = OperationExpectation::SplitCc {
                party: config.party_id.clone(),
                output_amounts: output_amounts.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::SplitCc as i32,
                        params: Some(Params::SplitCc(SplitCcParams {
                            output_amounts: output_amounts.clone(),
                            amulet_cids,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            if result.update_id.is_empty() {
                return Ok(());
            }
            println!("CC split completed: {}", result.update_id);
            for c in &result.created_contracts {
                if !c.amount.is_empty() {
                    println!("  Amulet {} (amount: {})", c.contract_id, c.amount);
                }
            }
        }
        TransferCommands::BatchPay {
            receivers,
            amounts,
            file,
            description,
            amulet_cids,
        } => {
            // Parse targets from CSV file or inline args
            let targets: Vec<(String, String)> = if let Some(csv_path) = file {
                parse_batch_pay_csv(&csv_path)?
            } else {
                if receivers.len() != amounts.len() {
                    return Err(anyhow::anyhow!(
                        "--receiver count ({}) must match --amount count ({})",
                        receivers.len(),
                        amounts.len()
                    ));
                }
                receivers.into_iter().zip(amounts.into_iter()).collect()
            };

            if targets.is_empty() {
                return Err(anyhow::anyhow!("No payment targets specified"));
            }

            // Parse strictly so the coverage check below sees real amounts.
            let mut total = rust_decimal::Decimal::ZERO;
            for (i, (party, amt)) in targets.iter().enumerate() {
                total += parse_payment_amount(amt, party, i + 1)?;
            }
            println!(
                "Batch pay: {} recipients, total {:.4} CC",
                targets.len(),
                total
            );
            for (i, (receiver, amount)) in targets.iter().enumerate() {
                let short_party = if receiver.len() > 24 {
                    format!("{}...{}", &receiver[..12], &receiver[receiver.len() - 8..])
                } else {
                    receiver.clone()
                };
                println!("  {:>3}. {} — {} CC", i + 1, short_party, amount);
            }

            // Auto-select amulets if not provided. Shares the payment queue's
            // selector so both paths keep the same ordering, cap and margin.
            let selected_amulet_cids = if let Some(cids) = amulet_cids {
                cids
            } else {
                let mut amulets = match client.get_amulets().await {
                    Ok(a) => a,
                    Err(_) => client.get_amulets_via_acs().await?,
                };
                amulets.sort_by(|a, b| a.amount.cmp(&b.amount));

                let amounts: Vec<rust_decimal::Decimal> =
                    amulets.iter().map(|a| a.amount).collect();

                // The margin is a preference, not a requirement: a wallet that
                // covers the payments themselves is still allowed to try.
                let target = payment_queue::with_fee_margin(total);
                let mut indices = payment_queue::select_amulet_indices(&amounts, target);
                let mut tight = false;
                if indices.is_empty() {
                    indices = payment_queue::select_amulet_indices(&amounts, total);
                    tight = !indices.is_empty();
                }

                if indices.is_empty() {
                    let available: rust_decimal::Decimal =
                        amounts.iter().copied().sum::<rust_decimal::Decimal>();
                    let considered = payment_queue::MAX_AMULET_INPUTS.min(amounts.len());
                    let best: rust_decimal::Decimal = {
                        let mut sorted = amounts.clone();
                        sorted.sort_by(|a, b| b.cmp(a));
                        sorted.iter().take(considered).copied().sum()
                    };
                    return Err(anyhow::anyhow!(
                        "Insufficient CC: payments total {:.4} but the {} largest of {} amulet(s) \
                         cover only {:.4} ({:.4} unlocked in total) — consolidate holdings or \
                         lower the amounts",
                        total,
                        considered,
                        amounts.len(),
                        best,
                        available
                    ));
                }

                let selected_total: rust_decimal::Decimal =
                    indices.iter().map(|&i| amulets[i].amount).sum();
                if tight {
                    println!(
                        "Note: selection covers the payments ({:.4} CC) but not the {:.4} CC fee \
                         margin — fees will be tight.",
                        total, target
                    );
                }
                println!(
                    "Auto-selected {} amulet(s) ({:.4} CC, target {:.4} incl. fee margin):",
                    indices.len(),
                    selected_total,
                    target
                );
                for &i in &indices {
                    let a = &amulets[i];
                    let short = if a.contract_id.len() > 24 {
                        format!(
                            "{}...{}",
                            &a.contract_id[..12],
                            &a.contract_id[a.contract_id.len() - 8..]
                        )
                    } else {
                        a.contract_id.clone()
                    };
                    println!("  {} ({:.4} CC)", short, a.amount);
                }

                indices
                    .into_iter()
                    .map(|i| amulets[i].contract_id.clone())
                    .collect()
            };

            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Batch Pay",
                    &format!("{} recipients, total {} CC", targets.len(), total),
                )
                .await?;
            }

            // Build proto targets
            let proto_targets: Vec<McPaymentTarget> = targets
                .iter()
                .map(|(receiver, amount)| McPaymentTarget {
                    receiver: receiver.clone(),
                    amount: amount.clone(),
                    description: description.clone(),
                })
                .collect();

            let op_count = 1; // single BatchPay operation
            let expectation = OperationExpectation::ExecuteMulticall {
                party: config.party_id.clone(),
                op_count,
            };

            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::ExecuteMulticall as i32,
                        params: Some(Params::ExecuteMulticall(ExecuteMultiCallParams {
                            operations: vec![MultiCallOp {
                                op: Some(orderbook_proto::ledger::multi_call_op::Op::BatchPay(
                                    McBatchPay {
                                        targets: proto_targets,
                                    },
                                )),
                            }],
                            holding_cids: selected_amulet_cids,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            if !result.update_id.is_empty() {
                println!("Batch pay completed: {}", result.update_id);
            }
        }
        TransferCommands::PrepayTraffic {
            amount,
            description,
        } => {
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Prepay traffic",
                    &format!("amount: {} CC", amount),
                )
                .await?;
            }
            let command_id = format!("cli-prepay-{}", uuid::Uuid::now_v7());
            let expectation = OperationExpectation::PrepayTraffic {
                sender_party: config.party_id.clone(),
                amount: amount.clone(),
                command_id: command_id.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::PrepayTraffic as i32,
                        params: Some(Params::PrepayTraffic(PrepayTrafficParams {
                            amount,
                            description,
                            command_id,
                            amulet_cids: vec![],
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            if result.update_id.is_empty() {
                return Ok(());
            }
            println!("Prepaid traffic top-up sent: {}", result.update_id);

            // Show post-topup balance so the user can confirm the credit
            // landed (record_credit fires after execute completes).
            match client.get_prepaid_traffic_balance().await {
                Ok(pt) => {
                    println!("\n=== Prepaid Traffic Balance ===\n");
                    println!("Balance:           {} CC", pt.balance_cc);
                    println!("Credit limit:      {} CC", pt.credit_limit_cc);
                    println!("Available:         {} CC", pt.available_cc);
                }
                Err(e) => {
                    println!("(unable to fetch post-topup balance: {})", e);
                }
            }
        }
    }

    Ok(())
}

/// A pending transfer contract left behind by the two-step path.
#[derive(Debug, PartialEq, Eq)]
pub enum PendingTransfer<'a> {
    /// Registry-issued offer, acceptable with `transfer accept-cip56`.
    RegistryOffer(&'a str),
    /// Amulet instruction, which the CIP-56 accept path cannot consume.
    AmuletInstruction(&'a str),
}

/// The pending transfer among a transaction's created contracts. A preapproved
/// receiver settles inline, leaving no such contract.
pub fn pending_transfer_offer(
    created: &[orderbook_proto::ledger::CreatedContractInfo],
) -> Option<PendingTransfer<'_>> {
    created.iter().find_map(|c| {
        if c.template_id.contains("AmuletTransferInstruction") {
            Some(PendingTransfer::AmuletInstruction(&c.contract_id))
        } else if c.template_id.contains("TransferOffer")
            || c.template_id.contains("TransferInstruction")
        {
            Some(PendingTransfer::RegistryOffer(&c.contract_id))
        } else {
            None
        }
    })
}

/// What a transaction's created contracts say about a transfer's outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum TransferOutcome<'a> {
    /// The receiver was preapproved and the transfer settled inline.
    CompletedInOneStep,
    /// A pending contract is waiting on the receiver.
    Pending(PendingTransfer<'a>),
    /// No created contracts came back, so nothing can be concluded.
    Unknown,
}

/// Classify a transfer's outcome. An empty `created` list is NOT evidence of
/// completion: the ledger update detail behind it is fetched best-effort.
pub fn transfer_outcome(
    created: &[orderbook_proto::ledger::CreatedContractInfo],
) -> TransferOutcome<'_> {
    if created.is_empty() {
        return TransferOutcome::Unknown;
    }
    match pending_transfer_offer(created) {
        Some(pending) => TransferOutcome::Pending(pending),
        None => TransferOutcome::CompletedInOneStep,
    }
}

/// Print what the created contracts say about a transfer's outcome.
fn report_transfer_outcome(created: &[orderbook_proto::ledger::CreatedContractInfo]) {
    match transfer_outcome(created) {
        TransferOutcome::Unknown => {
            println!("Outcome not confirmed — the ledger returned no update detail")
        }
        TransferOutcome::Pending(PendingTransfer::RegistryOffer(cid)) => {
            println!("Awaiting receiver acceptance — TransferOffer {}", cid);
            println!("  Receiver accepts with: transfer accept-cip56 --contract-id {}", cid);
        }
        TransferOutcome::Pending(PendingTransfer::AmuletInstruction(cid)) => {
            println!("Awaiting receiver acceptance — AmuletTransferInstruction {}", cid);
            println!("  Acceptance for this instrument is not served by accept-cip56.");
        }
        TransferOutcome::CompletedInOneStep => {
            println!("Completed in one step (receiver is preapproved)")
        }
    }
}

/// Parse one payment amount, naming the row so a bad value is actionable.
/// Amounts must be positive: a zero or negative leg cannot settle.
pub fn parse_payment_amount(
    amount: &str,
    party: &str,
    row: usize,
) -> Result<rust_decimal::Decimal> {
    let parsed = amount
        .parse::<rust_decimal::Decimal>()
        .map_err(|e| anyhow::anyhow!("row {row}: invalid amount '{amount}' for '{party}': {e}"))?;
    if parsed <= rust_decimal::Decimal::ZERO {
        return Err(anyhow::anyhow!(
            "row {row}: amount '{amount}' for '{party}' must be greater than zero"
        ));
    }
    Ok(parsed)
}

/// Parse a CSV file with batch payment targets.
/// Supports format: `Recipient Party ID,Amount` (header optional).
/// Also supports headerless `party_id,amount` rows.
pub fn parse_batch_pay_csv(path: &std::path::Path) -> Result<Vec<(String, String)>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .trim(csv::Trim::All)
        .from_path(path)
        .with_context(|| format!("Failed to open CSV file: {}", path.display()))?;

    let mut targets = Vec::new();
    for (i, record) in reader.records().enumerate() {
        let record = record.with_context(|| format!("Failed to parse CSV row {}", i + 1))?;
        if record.len() < 2 {
            return Err(anyhow::anyhow!(
                "CSV row {} has fewer than 2 columns",
                i + 1
            ));
        }
        let party = record[0].trim().to_string();
        let amount = record[1].trim().to_string();

        // Skip header row if present
        if i == 0
            && (party.contains("Party") || party.contains("party") || party.contains("Recipient"))
        {
            continue;
        }

        parse_payment_amount(&amount, &party, i + 1)
            .with_context(|| format!("CSV file {}", path.display()))?;

        targets.push((party, amount));
    }

    Ok(targets)
}

// ============================================================================
// Sign commands
// ============================================================================

pub fn run_generate_private_key() -> Result<()> {
    let (private_key, public_key) = agent_logic::sign::generate_keypair();
    println!("PARTY_AGENT_PRIVATE_KEY={}", private_key);
    println!("PARTY_AGENT_PUBLIC_KEY={}", public_key);
    Ok(())
}

// ============================================================================
// Onboard command — self-service agent onboarding
// ============================================================================

/// Resolve the CIP-56 operator party for a registry from the advertised list.
pub fn operator_for_registry(
    advertised: &[FaucetInstrument],
    registry: &str,
) -> Result<String> {
    advertised
        .iter()
        .find(|inst| inst.registry == registry)
        .map(|inst| inst.operator.clone())
        .filter(|op| !op.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "No preapproval target matches admin '{}'; cannot determine operator. \
                 Available registries: [{}]",
                registry,
                advertised
                    .iter()
                    .map(|i| i.registry.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        })
}

/// One CIP-56 preapproval that still needs creating.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreapprovalTarget {
    admin: String,
    operator: String,
    /// Instrument labels served by this admin, for logging only.
    labels: Vec<String>,
}

/// Advertised registries that still need a CIP-56 preapproval, plus warnings.
/// Targets are per admin party; only an unscoped one received by `party` counts.
fn preapproval_targets(
    existing: &[PreapprovalInfo],
    advertised: &[FaucetInstrument],
    dso_party: &str,
    party: &str,
) -> (Vec<PreapprovalTarget>, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();
    let mut targets: Vec<PreapprovalTarget> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for inst in advertised {
        if inst.token_name == "Amulet" || (!dso_party.is_empty() && inst.registry == dso_party) {
            continue;
        }
        if inst.registry.is_empty() {
            warnings.push(format!(
                "preapproval target '{}' has no registry; skipped",
                inst.token_name
            ));
            continue;
        }
        if inst.operator.is_empty() {
            warnings.push(format!(
                "registry {} has no operator; skipped",
                inst.registry
            ));
            continue;
        }
        if let Some(&i) = index.get(&inst.registry) {
            if targets[i].operator != inst.operator {
                warnings.push(format!(
                    "registry {} is advertised with two operators ({} and {}); using the first",
                    inst.registry, targets[i].operator, inst.operator
                ));
            }
            targets[i].labels.push(inst.token_name.clone());
        } else {
            index.insert(inst.registry.clone(), targets.len());
            targets.push(PreapprovalTarget {
                admin: inst.registry.clone(),
                operator: inst.operator.clone(),
                labels: vec![inst.token_name.clone()],
            });
        }
    }

    targets.retain(|t| {
        let mut scoped_only = false;
        for p in existing
            .iter()
            .filter(|p| p.receiver == party && p.instrument_admin == t.admin)
        {
            if !p.instrument_allowances.is_empty() {
                scoped_only = true;
                continue;
            }
            if p.operator != t.operator {
                warnings.push(format!(
                    "registry {} is already preapproved with operator {} (advertised: {}); left as \
                     is — use `preapproval request --instrument-admin {} --replace` to add one",
                    t.admin, p.operator, t.operator, t.admin
                ));
            }
            return false;
        }
        if scoped_only {
            warnings.push(format!(
                "registry {} is preapproved only for specific instruments; adding an unscoped one",
                t.admin
            ));
        }
        true
    });

    (targets, warnings)
}

/// What a Splice ACS snapshot says about CC preapproval coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CcCoverage {
    /// Accepted preapproval valid past the deadline.
    Active,
    /// Proposal still awaiting featured-app acceptance.
    ProposalPending,
    /// Absent, expired, or unreadable — treated as no coverage.
    Missing,
}

/// Classify `(template_id, expiresAt)` pairs from a Splice ACS snapshot.
fn classify_cc_coverage(
    entries: &[(String, Option<String>)],
    deadline: chrono::DateTime<chrono::Utc>,
) -> CcCoverage {
    let mut proposal = false;
    let mut stale_accepted = false;
    for (template_id, expires_at) in entries {
        if template_id.contains("TransferPreapprovalProposal") {
            proposal = true;
        } else if template_id.contains("TransferPreapproval") {
            if expires_at
                .as_deref()
                .and_then(parse_daml_timestamp)
                .is_some_and(|t| t > deadline)
            {
                return CcCoverage::Active;
            }
            stale_accepted = true;
        }
    }
    // A lingering proposal must not mask an expired preapproval.
    if proposal && !stale_accepted {
        CcCoverage::ProposalPending
    } else {
        CcCoverage::Missing
    }
}

/// Renew a CC preapproval this far ahead of its on-chain expiry.
fn cc_renewal_deadline(now: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    now + chrono::Duration::days(30)
}

/// Daml timestamps arrive as RFC 3339 or as microseconds since the epoch.
fn parse_daml_timestamp(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(t.with_timezone(&chrono::Utc));
    }
    raw.trim()
        .parse::<i64>()
        .ok()
        .and_then(chrono::DateTime::from_timestamp_micros)
}

/// Read a top-level string field off a contract's create arguments.
fn struct_field_string(
    contract: &orderbook_proto::ledger::ActiveContractInfo,
    field: &str,
) -> Option<String> {
    match &contract.create_arguments.as_ref()?.fields.get(field)?.kind {
        Some(prost_types::value::Kind::StringValue(s)) => Some(s.clone()),
        Some(prost_types::value::Kind::NumberValue(n)) => Some(format!("{n:.0}")),
        _ => None,
    }
}

/// Read a key=value from .env file, returning None if not found or file doesn't exist.
/// Surrounding quotes are stripped, matching what `write_server_config_to_env` writes.
pub fn read_env_value(env_file: &std::path::Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(env_file).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(val) = line.strip_prefix(key) {
            if let Some(val) = val.strip_prefix('=') {
                let val = val.trim();
                let val = val
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .or_else(|| val.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                    .unwrap_or(val);
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Write or update a key=value in the .env file
pub fn upsert_env_value(env_file: &std::path::Path, key: &str, value: &str) -> Result<()> {
    let mut lines: Vec<String> = if env_file.exists() {
        std::fs::read_to_string(env_file)
            .context("Failed to read .env file")?
            .lines()
            .map(|l| l.to_string())
            .collect()
    } else {
        Vec::new()
    };

    let prefix = format!("{}=", key);
    let new_line = format!("{}={}", key, value);
    let mut found = false;
    for line in &mut lines {
        if line.starts_with(&prefix) {
            *line = new_line.clone();
            found = true;
            break;
        }
    }
    if !found {
        lines.push(new_line);
    }

    std::fs::write(env_file, lines.join("\n") + "\n").context("Failed to write .env file")?;
    Ok(())
}

/// Write all server config values from GetAgentConfigResponse to .env
pub fn write_server_config_to_env(
    env_file: &std::path::Path,
    rpc: &str,
    config_resp: &GetAgentConfigResponse,
) -> Result<()> {
    // Write all KEY=VALUE pairs from the server's .env.agent
    for line in config_resp.env.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            // Don't overwrite agent-specific keys
            if matches!(
                key,
                "PARTY_AGENT"
                    | "PARTY_AGENT_PRIVATE_KEY"
                    | "PARTY_AGENT_PUBLIC_KEY"
                    | "ORDERBOOK_GRPC_URL"
            ) {
                continue;
            }
            upsert_env_value(env_file, key, value)?;
        }
    }
    // Always set the RPC URL from the --rpc flag
    upsert_env_value(env_file, "ORDERBOOK_GRPC_URL", rpc)?;
    Ok(())
}

/// Save agent.toml from server config if it doesn't already exist locally.
/// Customizes `name = "LP agent"` → `name = "LP <agent_name>"` if agent_name is provided.
pub fn maybe_write_agent_toml(
    env_file: &std::path::Path,
    config_resp: &GetAgentConfigResponse,
    agent_name: Option<&str>,
) {
    if config_resp.agent_toml.is_empty() {
        return;
    }
    let toml_path = env_file
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("agent.toml");
    if toml_path.exists() {
        println!("agent.toml already exists, skipping write");
        return;
    }
    let mut content = config_resp.agent_toml.clone();
    if let Some(name) = agent_name {
        content = content.replace("name = \"LP agent\"", &format!("name = \"LP {}\"", name));
    }
    match std::fs::write(&toml_path, &content) {
        Ok(_) => println!("Agent config written to {}", toml_path.display()),
        Err(e) => eprintln!("Warning: could not write agent.toml: {}", e),
    }
}

/// Create a raw (unauthenticated) gRPC channel
pub async fn create_raw_channel(grpc_url: &str) -> Result<tonic::transport::Channel> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    if grpc_url.starts_with("https://") {
        let tls_config = tonic::transport::ClientTlsConfig::new()
            .with_webpki_roots()
            .domain_name(
                grpc_url
                    .trim_start_matches("https://")
                    .split(':')
                    .next()
                    .unwrap_or("localhost"),
            );

        tonic::transport::Channel::from_shared(grpc_url.to_string())
            .context("Invalid gRPC URL")?
            .tls_config(tls_config)
            .context("Failed to configure TLS")?
            .connect()
            .await
            .context("Failed to connect to gRPC service")
    } else {
        tonic::transport::Channel::from_shared(grpc_url.to_string())
            .context("Invalid gRPC URL")?
            .connect()
            .await
            .context("Failed to connect to gRPC service")
    }
}

/// Sign an onboarding RPC request using the agent's private key
pub fn sign_onboarding_request(private_key_bytes: &[u8; 32], canonical: &[u8]) -> MessageSignature {
    let sig_data = message_signing::sign_canonical(private_key_bytes, canonical);
    MessageSignature {
        signature: sig_data.signature_b64,
        public_key: sig_data.public_key_b64url,
        signing_scheme: sig_data.signing_scheme,
    }
}

pub async fn run_onboard(
    rpc: String,
    party: Option<String>,
    private_key: Option<agent_logic::secret::Zeroizing<String>>,
    invite_code: String,
    agent_name: String,
    email: String,
    env_file: PathBuf,
    poll_interval: u64,
) -> Result<()> {
    println!("=== Cloud Agent Self-Service Onboarding ===\n");

    // Step 1: Handle keys
    let (_, effective_key) = prepare_onboard_key(&env_file, private_key.as_ref(), &rpc)?;

    // If --party provided, write it and skip waiting list entirely
    if let Some(ref party_id) = party {
        upsert_env_value(&env_file, "PARTY_AGENT", party_id)?;
        println!("PARTY_AGENT={} (from --party flag)", party_id);
    }

    // Check if already fully onboarded (either from --party or from .env)
    if let Some(party_id) = read_env_value(&env_file, "PARTY_AGENT") {
        if !party_id.is_empty() {
            // Still need server config — fetch it if not already in .env
            if read_env_value(&env_file, "SYNCHRONIZER_ID").is_none() {
                println!("\nFetching server configuration...");
                let channel = create_raw_channel(&rpc).await?;
                let mut client = DAppProviderServiceClient::new(channel)
                    .max_decoding_message_size(16 * 1024 * 1024);
                let config_resp = client
                    .get_agent_config(GetAgentConfigRequest {})
                    .await
                    .context("GetAgentConfig RPC failed")?
                    .into_inner();
                write_server_config_to_env(&env_file, &rpc, &config_resp)?;
                println!("Server config written to {}", env_file.display());
                maybe_write_agent_toml(&env_file, &config_resp, Some(&agent_name));
            }
            println!("\nPARTY_AGENT={} — checking ledger onboarding...", party_id);
            return complete_ledger_onboarding(&env_file, &rpc, Some(&party_id), Some(&effective_key))
                .await;
        }
    }

    // Need private key bytes and public key for the waiting list flow
    let private_key_bytes = agent_logic::config::decode_private_key(&effective_key)?;
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&private_key_bytes);
    let public_key_b58 = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();

    // Step 2: Connect to RPC (raw channel, no JWT auth needed)
    println!("\nConnecting to {}...", rpc);
    let channel = create_raw_channel(&rpc).await?;
    let mut client =
        DAppProviderServiceClient::new(channel).max_decoding_message_size(16 * 1024 * 1024);
    println!("Connected.");

    // Step 3: Fetch server config
    println!("\nFetching server configuration...");
    let config_resp = client
        .get_agent_config(GetAgentConfigRequest {})
        .await
        .context("GetAgentConfig RPC failed")?
        .into_inner();

    write_server_config_to_env(&env_file, &rpc, &config_resp)?;
    println!("Server config written to {}", env_file.display());
    maybe_write_agent_toml(&env_file, &config_resp, Some(&agent_name));

    // Step 4: Register on waiting list (idempotent)
    println!("\nRegistering agent on waiting list...");
    let canonical = message_signing::canonical_register_agent(&public_key_b58, Some(&invite_code));
    let sig = sign_onboarding_request(&private_key_bytes, &canonical);

    let register_resp = client
        .register_agent(RegisterAgentRequest {
            public_key: public_key_b58.clone(),
            invite_code: Some(invite_code),
            email: Some(email.clone()),
            agent_name: Some(agent_name.clone()),
            request_signature: Some(sig),
        })
        .await
        .context("RegisterAgent RPC failed")?
        .into_inner();

    println!(
        "Registration: {} (id={})",
        register_resp.message, register_resp.waiting_list_id
    );

    // Step 5: Poll for SIGNATURE_REQUIRED status
    let mut current_status = register_resp.status;
    loop {
        if current_status == ProtoOnboardingStatus::SignatureRequired as i32
            || current_status == ProtoOnboardingStatus::TopologyCreated as i32
            || current_status == ProtoOnboardingStatus::Onboarded as i32
        {
            break;
        }

        println!(
            "Status: REQUESTED — waiting for backend to create multihash (polling every {}s)...",
            poll_interval
        );
        tokio::time::sleep(std::time::Duration::from_secs(poll_interval)).await;

        let canonical = message_signing::canonical_get_onboarding_status(&public_key_b58);
        let sig = sign_onboarding_request(&private_key_bytes, &canonical);

        let status_resp = client
            .get_onboarding_status(GetOnboardingStatusRequest {
                public_key: public_key_b58.clone(),
                request_signature: Some(sig),
            })
            .await
            .context("GetOnboardingStatus RPC failed")?
            .into_inner();

        current_status = status_resp.status;
    }

    // Step 6: Sign multihash if needed
    if current_status == ProtoOnboardingStatus::SignatureRequired as i32 {
        // Fetch the multihash
        let canonical = message_signing::canonical_get_onboarding_status(&public_key_b58);
        let sig = sign_onboarding_request(&private_key_bytes, &canonical);

        let status_resp = client
            .get_onboarding_status(GetOnboardingStatusRequest {
                public_key: public_key_b58.clone(),
                request_signature: Some(sig),
            })
            .await
            .context("GetOnboardingStatus RPC failed")?
            .into_inner();

        let multihash = status_resp
            .multihash
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("Status is SIGNATURE_REQUIRED but no multihash was provided")
            })?;

        println!("\nSigning multihash...");
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&private_key_bytes);
        let multihash_signature = agent_logic::sign::sign_multihash(&signing_key, &multihash)?;

        let canonical = message_signing::canonical_submit_onboarding_signature(
            &public_key_b58,
            &multihash_signature,
        );
        let sig = sign_onboarding_request(&private_key_bytes, &canonical);

        let submit_resp = client
            .submit_onboarding_signature(SubmitOnboardingSignatureRequest {
                public_key: public_key_b58.clone(),
                multihash_signature,
                request_signature: Some(sig),
            })
            .await
            .context("SubmitOnboardingSignature RPC failed")?
            .into_inner();

        println!("Signature submitted: {}", submit_resp.message);
        current_status = submit_resp.status;
    }

    // Step 7: Poll for TOPOLOGY_CREATED status
    loop {
        if current_status == ProtoOnboardingStatus::TopologyCreated as i32
            || current_status == ProtoOnboardingStatus::Onboarded as i32
        {
            break;
        }

        println!(
            "Status: SIGNATURE_SUBMITTED — waiting for topology creation (polling every {}s)...",
            poll_interval
        );
        tokio::time::sleep(std::time::Duration::from_secs(poll_interval)).await;

        let canonical = message_signing::canonical_get_onboarding_status(&public_key_b58);
        let sig = sign_onboarding_request(&private_key_bytes, &canonical);

        let status_resp = client
            .get_onboarding_status(GetOnboardingStatusRequest {
                public_key: public_key_b58.clone(),
                request_signature: Some(sig),
            })
            .await
            .context("GetOnboardingStatus RPC failed")?
            .into_inner();

        current_status = status_resp.status;

        if current_status == ProtoOnboardingStatus::TopologyCreated as i32
            || current_status == ProtoOnboardingStatus::Onboarded as i32
        {
            let party_id = status_resp
                .party_id
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("Status is TOPOLOGY_CREATED but no party_id was provided")
                })?;
            // Step 8: Write party_id to .env
            upsert_env_value(&env_file, "PARTY_AGENT", &party_id)?;
            println!("\nTopology created! PARTY_AGENT={}", party_id);
            break;
        }
    }

    // If we got here from a status that already had topology, fetch party_id
    if read_env_value(&env_file, "PARTY_AGENT").is_none() {
        let canonical = message_signing::canonical_get_onboarding_status(&public_key_b58);
        let sig = sign_onboarding_request(&private_key_bytes, &canonical);

        let status_resp = client
            .get_onboarding_status(GetOnboardingStatusRequest {
                public_key: public_key_b58.clone(),
                request_signature: Some(sig),
            })
            .await
            .context("GetOnboardingStatus RPC failed")?
            .into_inner();

        let party_id = status_resp
            .party_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Onboarding completed but no party_id was provided"))?;
        upsert_env_value(&env_file, "PARTY_AGENT", &party_id)?;
        println!("\nPARTY_AGENT={}", party_id);
    }

    // Steps 9-11: Complete ledger onboarding
    let party_id = read_env_value(&env_file, "PARTY_AGENT");
    complete_ledger_onboarding(&env_file, &rpc, party_id.as_deref(), Some(&effective_key)).await
}

/// Onboarding Step 1: resolve the agent key from the supplied value, the env
/// file, or a fresh keypair. Returns the public key and the key to sign with.
fn prepare_onboard_key(
    env_file: &std::path::Path,
    supplied: Option<&agent_logic::secret::Zeroizing<String>>,
    rpc: &str,
) -> Result<(String, agent_logic::secret::Zeroizing<String>)> {
    use agent_logic::secret::Zeroizing;
    let env_value = |name: &str| read_env_value(env_file, name).filter(|v| !v.is_empty());
    let public_key_of = |b58: &str| -> Result<String> {
        let bytes = agent_logic::config::decode_private_key(b58)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&bytes);
        Ok(bs58::encode(signing_key.verifying_key().as_bytes()).into_string())
    };

    if let Some(pk_b58) = supplied {
        // Key supplied via flag, environment or prompt: derive the public key, keep the key out of .env
        let pub_b58 = public_key_of(pk_b58)?;
        if env_value("PARTY_AGENT_PUBLIC_KEY").is_some_and(|existing| existing != pub_b58) {
            anyhow::bail!(
                "the supplied private key does not match PARTY_AGENT_PUBLIC_KEY in {}",
                env_file.display()
            );
        }
        match env_value("PARTY_AGENT_PRIVATE_KEY") {
            Some(existing) if existing.trim() != pk_b58.trim() => anyhow::bail!(
                "{} already holds a different PARTY_AGENT_PRIVATE_KEY — remove it or omit --private-key",
                env_file.display()
            ),
            Some(_) => {}
            None => println!(
                "Private key not written to {} — pass --private-key or enter it when prompted.",
                env_file.display()
            ),
        }
        upsert_env_value(env_file, "PARTY_AGENT_PUBLIC_KEY", &pub_b58)?;
        upsert_env_value(env_file, "ORDERBOOK_GRPC_URL", rpc)?;
        println!("Public key: {}", pub_b58);
        Ok((pub_b58, pk_b58.clone()))
    } else if let Some(pk) = env_value("PARTY_AGENT_PRIVATE_KEY") {
        // Existing private key in .env
        println!("Found existing private key in {}", env_file.display());
        let pk = Zeroizing::new(pk);
        let pub_b58 = public_key_of(&pk)?;
        upsert_env_value(env_file, "PARTY_AGENT_PUBLIC_KEY", &pub_b58)?;
        println!("Public key: {}", pub_b58);
        Ok((pub_b58, pk))
    } else if env_value("PARTY_AGENT").is_some() || env_value("PARTY_AGENT_PUBLIC_KEY").is_some() {
        anyhow::bail!(
            "{} already records an identity but no PARTY_AGENT_PRIVATE_KEY — \
             pass --private-key or run in a terminal to be prompted",
            env_file.display()
        );
    } else {
        // No key provided and none in .env — generate new keypair
        println!("Generating new Ed25519 keypair...");
        let (priv_b58, pub_b58) = agent_logic::sign::generate_keypair();
        let priv_b58 = Zeroizing::new(priv_b58);
        upsert_env_value(env_file, "PARTY_AGENT_PRIVATE_KEY", &priv_b58)?;
        upsert_env_value(env_file, "PARTY_AGENT_PUBLIC_KEY", &pub_b58)?;
        upsert_env_value(env_file, "ORDERBOOK_GRPC_URL", rpc)?;
        println!("Private key written to {}", env_file.display());
        println!("Public key: {}", pub_b58);
        Ok((pub_b58, priv_b58))
    }
}

/// Minimal config for the `onboard` CLI path.
///
/// The full `BaseConfig` is driven by `agent.toml` + `.env` + `configuration.toml`
/// and is needed for the `cloud-agent agent` runtime (markets, LP settings, etc.).
/// The `onboard` subcommand only needs identity + endpoint info, all of which live
/// in `.env` after Step 2 of `run_onboard`. Using this struct lets the onboard
/// path run without any `agent.toml` at all.
pub struct OnboardConfig {
    pub party_id: String,
    pub role: String,
    pub private_key: agent_logic::secret::Secret<32>,
    pub node_name: String,
    pub ledger_service_public_key: [u8; 32],
    pub token_ttl_secs: u64,
    pub connection_timeout_secs: u64,
    pub request_timeout_secs: u64,
}

impl OnboardConfig {
    /// Env-driven config; the overrides (flags, prompt, freshly written .env
    /// values) win over `PARTY_AGENT` / `PARTY_AGENT_PRIVATE_KEY`.
    pub fn from_env_with(
        party_override: Option<&str>,
        key_override: Option<&agent_logic::secret::Zeroizing<String>>,
    ) -> Result<Self> {
        let party_id = match party_override {
            Some(p) => p.to_string(),
            None => std::env::var("PARTY_AGENT")
                .map_err(|_| anyhow::anyhow!("PARTY_AGENT env var is required"))?,
        };
        let private_key_base58 = match key_override {
            Some(k) => agent_logic::secret::Zeroizing::new(k.to_string()),
            None => agent_logic::secret::Zeroizing::new(
                std::env::var("PARTY_AGENT_PRIVATE_KEY").map_err(|_| {
                    anyhow::anyhow!("PARTY_AGENT_PRIVATE_KEY env var (or --private-key) is required")
                })?,
            ),
        };
        let mut private_key_bytes = agent_logic::config::decode_private_key(&private_key_base58)?;
        let private_key = agent_logic::secret::Secret::seal(&mut private_key_bytes);
        let node_name = std::env::var("NODE_NAME")
            .map_err(|_| anyhow::anyhow!("NODE_NAME env var is required"))?;
        let ledger_service_public_key_base58 = std::env::var("LEDGER_SERVICE_PUBLIC_KEY")
            .map_err(|_| anyhow::anyhow!("LEDGER_SERVICE_PUBLIC_KEY env var is required"))?;
        let ledger_service_public_key =
            agent_logic::config::decode_public_key(&ledger_service_public_key_base58)?;

        Ok(Self {
            party_id,
            role: std::env::var("AGENT_ROLE").unwrap_or_else(|_| "agent".to_string()),
            private_key,
            node_name,
            ledger_service_public_key,
            token_ttl_secs: 3600,
            connection_timeout_secs: 30,
            request_timeout_secs: 120,
        })
    }
}

/// Complete ledger onboarding (preapproval + user-service).
/// Called after PARTY_AGENT is set in .env. Does not require `agent.toml`.
pub async fn complete_ledger_onboarding(
    env_file: &std::path::Path,
    rpc: &str,
    party_override: Option<&str>,
    key_override: Option<&agent_logic::secret::Zeroizing<String>>,
) -> Result<()> {
    // Ensure .env is loaded into process env
    let _ = dotenvy::from_path(env_file);

    let cfg = OnboardConfig::from_env_with(party_override, key_override)
        .context("Failed to load onboarding config from .env")?;

    println!(
        "\nCompleting ledger onboarding for party {}...",
        cfg.party_id
    );

    let mut client = DAppProviderClient::new(
        rpc,
        &cfg.party_id,
        &cfg.role,
        &cfg.private_key,
        cfg.token_ttl_secs,
        Some(cfg.node_name.as_str()),
        &cfg.ledger_service_public_key,
        Some(cfg.connection_timeout_secs),
        Some(cfg.request_timeout_secs),
    )
    .await
    .context("Failed to create authenticated DAppProvider client")?;

    // Template IDs for on-chain contract lookups
    const TMPL_USER_SERVICE: &str =
        "#utility-settlement-app-v1:Utility.Settlement.App.V1.Service.User:UserService";
    const TMPL_USER_SERVICE_REQUEST: &str =
        "#utility-settlement-app-v1:Utility.Settlement.App.V1.Service.User:UserServiceRequest";

    // Fetch the faucet-supported instrument list once. Drives both the Splice
    // CC preapproval (Amulet entry's operator = PREAPPROVAL_FEATURED_APP) and
    // the per-registry CIP-56 preapprovals below.
    let faucet_instruments = client
        .list_faucet_instruments()
        .await
        .context("Failed to fetch faucet instruments from payments-rpc")?;

    // The Amulet entry carries both the CC admin (DSO) and the featured-app
    // operator; the DSO party also keeps CC out of the CIP-56 loop below.
    let amulet_entry = faucet_instruments
        .iter()
        .find(|i| i.token_name == "Amulet")
        .filter(|i| !i.registry.is_empty() && !i.operator.is_empty());
    let cc_dso_party = amulet_entry.map(|i| i.registry.clone()).unwrap_or_default();
    // Every preapproval that could not be created, reported once at the end.
    let mut preapproval_failures: Vec<String> = Vec::new();

    // 11a-CC: Create Splice TransferPreapproval for CC (required for CC auto-completion).
    // Check if Splice preapproval or proposal already exists.
    let splice_preapproval_templates = [
        "#splice-amulet:Splice.AmuletRules:TransferPreapproval".to_string(),
        "#splice-wallet:Splice.Wallet.TransferPreapproval:TransferPreapprovalProposal".to_string(),
    ];
    // An unreadable snapshot must neither look like coverage nor drive a
    // duplicate create, so the CC step is skipped and recorded instead.
    let cc_coverage = match client.get_active_contracts(&splice_preapproval_templates).await {
        Ok(contracts) => {
            let entries: Vec<(String, Option<String>)> = contracts
                .iter()
                .map(|c| (c.template_id.clone(), struct_field_string(c, "expiresAt")))
                .collect();
            // Renew ahead of expiry: Splice asserts expiresAt when the transfer runs.
            Some(classify_cc_coverage(&entries, cc_renewal_deadline(chrono::Utc::now())))
        }
        Err(e) => {
            println!("Warning: could not query existing Splice CC preapprovals: {:#}", e);
            preapproval_failures.push("CC (Splice): existing-preapproval query failed".to_string());
            None
        }
    };
    if cc_coverage == Some(CcCoverage::ProposalPending) {
        println!(
            "Splice CC preapproval proposal is pending featured-app acceptance; not creating another."
        );
    }
    if cc_coverage == Some(CcCoverage::Missing) {
        if amulet_entry.is_none() {
            println!(
                "Warning: no usable Amulet entry in the preapproval target list; skipping CC preapproval."
            );
            preapproval_failures.push("CC (Splice): no Amulet preapproval target".to_string());
        } else {
            let amulet = amulet_entry.expect("checked above");
            let dso_for_cc = amulet.registry.clone();
            let amulet_operator = amulet.operator.clone();
            println!("Creating Splice preapproval for CC...");
            let expectation = OperationExpectation::RequestPreapproval {
                party: cfg.party_id.clone(),
            };
            match client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestPreapproval as i32,
                        params: Some(Params::RequestPreapproval(RequestPreapprovalParams {
                            instrument_admin: dso_for_cc,
                            instrument_allowances: vec![],
                            operator: amulet_operator,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    false,
                    false,
                    false,
                )
                .await
            {
                Ok(_) => println!(
                    "Splice preapproval proposal created for CC (pending featured-app acceptance)."
                ),
                Err(e) => {
                    println!("Warning: failed to create CC preapproval: {:#}", e);
                    preapproval_failures.push("CC (Splice): creation failed".to_string());
                }
            }
        }
    } else if cc_coverage == Some(CcCoverage::Active) {
        println!("Splice CC preapproval already exists and is current.");
    }

    // 11a-CIP56: Create CIP-56 TransferPreapprovals for utility tokens.
    let preapprovals = client.get_preapprovals().await?;
    let (targets, target_warnings) =
        preapproval_targets(&preapprovals, &faucet_instruments, &cc_dso_party, &cfg.party_id);
    for w in &target_warnings {
        println!("Warning: {}", w);
    }
    // Count registries, not entries: several instruments may share an admin.
    let advertised_utility = faucet_instruments
        .iter()
        .filter(|i| i.token_name != "Amulet" && !i.registry.is_empty())
        .map(|i| i.registry.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();

    if advertised_utility == 0 {
        println!(
            "Warning: the server advertises no utility preapproval targets; none can be created."
        );
    } else if targets.is_empty() {
        println!(
            "CIP-56 preapprovals up to date ({} registry/registries advertised).",
            advertised_utility
        );
    } else {
        println!(
            "Creating {} CIP-56 preapproval(s) of {} advertised registry/registries...",
            targets.len(),
            advertised_utility
        );
        for t in &targets {
            println!(
                "Creating CIP-56 preapproval for admin={} (instruments: {})...",
                t.admin,
                t.labels.join(", ")
            );
            let expectation = OperationExpectation::RequestPreapproval {
                party: cfg.party_id.clone(),
            };
            match client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestPreapproval as i32,
                        params: Some(Params::RequestPreapproval(RequestPreapprovalParams {
                            instrument_admin: t.admin.clone(),
                            instrument_allowances: vec![],
                            operator: t.operator.clone(),
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    false,
                    false,
                    false,
                )
                .await
            {
                Ok(_) => println!("CIP-56 preapproval created for admin={}.", t.admin),
                Err(e) => {
                    println!(
                        "Warning: failed to create CIP-56 preapproval for admin={}: {:#}",
                        t.admin, e
                    );
                    preapproval_failures.push(t.admin.clone());
                }
            }
        }
    }

    // 11b: Request user-service (check completed UserService AND pending UserServiceRequest)
    let user_contracts = client
        .get_active_contracts(&[
            TMPL_USER_SERVICE.to_string(),
            TMPL_USER_SERVICE_REQUEST.to_string(),
        ])
        .await?;
    if user_contracts.is_empty() {
        println!("Requesting user service...");
        let expectation = OperationExpectation::RequestUserService {
            party: cfg.party_id.clone(),
        };
        client
            .submit_transaction(
                PrepareTransactionRequest {
                    operation: TransactionOperation::RequestUserService as i32,
                    params: Some(Params::RequestUserService(RequestUserServiceParams {
                        reference_id: None,
                        party_name: None,
                    })),
                    request_signature: None,
                },
                &expectation,
                false,
                false,
                false,
            )
            .await
            .context("Failed to request user service")?;
        println!("User service requested.");
    } else {
        println!(
            "User service already exists or pending ({} contracts found).",
            user_contracts.len()
        );
    }

    // Devnet: auto-faucet CC and USDC to bootstrap agent balance
    let canton_chain = read_env_value(env_file, "CANTON_CHAIN").unwrap_or_default();
    if canton_chain == "devnet" {
        let balances = client.get_balances().await.unwrap_or_default();
        // Anything in faucet_instruments that's neither already funded nor Amulet runs first;
        // CC (Amulet) runs last because Splice preapproval may still be pending operator acceptance.
        let needs_faucet: Vec<&FaucetInstrument> = faucet_instruments
            .iter()
            .filter(|inst| {
                if inst.token_name == "Amulet" {
                    !balances.iter().any(|b| b.is_canton_coin)
                } else {
                    !balances.iter().any(|b| b.instrument_id == inst.token_name)
                }
            })
            .collect();
        let has_cc = !needs_faucet.iter().any(|inst| inst.token_name == "Amulet");

        if !needs_faucet.is_empty() {
            println!("\nDevnet detected — requesting faucet top-up...");

            // Utility tokens first (CIP-56 preapproval is already active, no waiting needed)
            for inst in needs_faucet.iter().filter(|i| i.token_name != "Amulet") {
                print!("  {}... ", inst.token_name);
                match client
                    .request_faucet(FaucetRequest {
                        token_name: inst.token_name.clone(),
                        token_admin: inst.registry.clone(),
                        ticket: String::new(),
                        amount: String::new(),
                        dry_run: false,
                        request_signature: None,
                    })
                    .await
                {
                    Ok(r) if r.success => println!("OK ({})", r.amount_approved),
                    Ok(r) => println!("failed: {}", r.error_message.unwrap_or_default()),
                    Err(e) => println!("error: {}", e),
                }
            }

            // CC last with retry (Splice preapproval may still be pending operator acceptance)
            if let Some(cc_inst) = needs_faucet.iter().find(|i| i.token_name == "Amulet") {
                let mut cc_ok = false;
                let delays = [0u64, 10, 15, 20, 25, 30, 30, 30, 30];
                for attempt in 0..delays.len() as u64 {
                    if attempt > 0 {
                        let delay = delays[attempt as usize];
                        println!(
                            "  CC: retrying in {}s (waiting for preapproval acceptance)...",
                            delay
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    }
                    print!("  CC (Amulet)... ");
                    match client
                        .request_faucet(FaucetRequest {
                            token_name: cc_inst.token_name.clone(),
                            token_admin: cc_inst.registry.clone(),
                            ticket: String::new(),
                            amount: String::new(),
                            dry_run: false,
                            request_signature: None,
                        })
                        .await
                    {
                        Ok(r) if r.success => {
                            println!("OK ({})", r.amount_approved);
                            cc_ok = true;
                            break;
                        }
                        Ok(r) => println!("failed: {}", r.error_message.unwrap_or_default()),
                        Err(e) => println!("error: {}", e),
                    }
                }
                if !cc_ok {
                    println!(
                        "  CC faucet failed after retries. Run './cloud-agent faucet get --token CC' manually."
                    );
                }
            }

            // Wait for balances
            println!("\nWaiting for balances...");
            for _ in 0..6 {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let balances = client.get_balances().await.unwrap_or_default();
                if !balances.is_empty() {
                    println!("\n=== Initial Balances ===");
                    for b in &balances {
                        let name = if b.is_canton_coin {
                            "CC".to_string()
                        } else {
                            b.instrument_id.clone()
                        };
                        println!("  {}: {}", name, b.total_amount);
                    }
                    break;
                }
            }
        } else if has_cc {
            println!("\nBalances already funded, skipping faucet.");
        }
    }

    // First topup of the off-chain prepaid traffic balance. Without this,
    // a fresh agent boots with zero prepaid balance and the very first
    // transaction hits a `Deny` at the canton-agent's preflight. The
    // per-tx auto-topup hook only fires AFTER a successful submit, so it
    // can't seed the initial balance — onboarding has to.
    let min_env = std::env::var("MIN_PREPAID_TRAFFIC_BALANCE_CC").ok();
    let topup_env = std::env::var("PREPAID_TRAFFIC_TOPUP_CC").ok();
    match (min_env, topup_env) {
        (Some(min_str), Some(topup_str)) => {
            let min_cc: rust_decimal::Decimal = min_str.parse().with_context(|| {
                format!(
                    "MIN_PREPAID_TRAFFIC_BALANCE_CC must be a decimal, got '{}'",
                    min_str
                )
            })?;
            let topup_cc: rust_decimal::Decimal = topup_str.parse().with_context(|| {
                format!(
                    "PREPAID_TRAFFIC_TOPUP_CC must be a decimal, got '{}'",
                    topup_str
                )
            })?;
            if topup_cc <= rust_decimal::Decimal::ZERO {
                return Err(anyhow!(
                    "PREPAID_TRAFFIC_TOPUP_CC must be > 0, got {}",
                    topup_cc
                ));
            }

            // Build a dedicated client for the topup. The onboarding
            // `client` above is consumed by the surrounding flow; using a
            // fresh client mirrors the runtime wiring and avoids touching
            // its post-success hook (none here, but safer either way).
            let topup_client = DAppProviderClient::new(
                rpc,
                &cfg.party_id,
                &cfg.role,
                &cfg.private_key,
                cfg.token_ttl_secs,
                Some(cfg.node_name.as_str()),
                &cfg.ledger_service_public_key,
                Some(cfg.connection_timeout_secs),
                Some(cfg.request_timeout_secs),
            )
            .await
            .context("Failed to create topup client for onboarding first-topup")?;

            let runner = topup::TopupRunner::new(
                topup_client,
                cfg.party_id.clone(),
                Some(min_cc),
                Some(topup_cc),
            )
            .expect("env vars validated above");

            if canton_chain == "devnet" {
                // Devnet: faucet above funded the agent's CC amulet wallet, so an
                // on-chain PrepayTraffic to seed the prepaid pool succeeds here.
                println!(
                    "\nSeeding prepaid traffic balance with first topup of {} CC...",
                    topup_cc
                );
                match runner.force_topup().await {
                    Ok(()) => match runner.get_balance().await {
                        Ok(pt) => {
                            println!("\n=== Initial Prepaid Traffic Balance ===");
                            println!("  Balance:      {} CC", pt.balance_cc);
                            println!("  Credit limit: {} CC", pt.credit_limit_cc);
                            println!("  Available:    {} CC", pt.available_cc);
                        }
                        Err(e) => {
                            println!("(unable to fetch post-topup prepaid balance: {})", e);
                        }
                    },
                    Err(e) => {
                        println!("First topup failed: {}", e);
                    }
                }
            } else {
                // Non-devnet (mainnet, etc.): no faucet ran, agent has 0 CC amulets,
                // so an on-chain PrepayTraffic would fail. The server's
                // ONBOARD_PREPAID_TRAFFIC_CC seed has already written a row to
                // prepaid_traffic_balance, so the agent boots with a working seed.
                // The runtime per-tx topup hook will refill once the operator
                // funds the agent's amulet wallet.
                println!(
                    "\nSkipping first topup (chain={}): no auto-faucet, agent has no CC amulets yet.",
                    canton_chain
                );
                println!(
                    "  To enable self-topup: send CC amulets to {} — the runtime hook will refill \
                     whenever balance drops below {} CC.",
                    cfg.party_id, min_cc
                );
                match runner.get_balance().await {
                    Ok(pt) => {
                        println!("\n=== Initial Prepaid Traffic Balance (server-seeded) ===");
                        println!("  Balance:      {} CC", pt.balance_cc);
                        println!("  Credit limit: {} CC", pt.credit_limit_cc);
                        println!("  Available:    {} CC", pt.available_cc);
                    }
                    Err(e) => println!("(unable to fetch prepaid balance: {})", e),
                }
            }
        }
        (None, None) => {
            println!(
                "\nAuto-topup disabled (MIN_PREPAID_TRAFFIC_BALANCE_CC / PREPAID_TRAFFIC_TOPUP_CC not set).\n\
                 Agent will run without auto-topup; fund the prepaid pool manually via `transfer prepay-traffic`."
            );
        }
        _ => {
            return Err(anyhow!(
                "MIN_PREPAID_TRAFFIC_BALANCE_CC and PREPAID_TRAFFIC_TOPUP_CC must both be set (or both unset) — both required for auto-topup"
            ));
        }
    }

    // Reported last so a single unreachable registry cannot cost the agent its
    // user service, faucet and traffic seed.
    if !preapproval_failures.is_empty() {
        anyhow::bail!(
            "onboarding finished, but {} CIP-56 preapproval(s) failed — retry with \
             `preapproval request --instrument-admin <PARTY>` for: {}",
            preapproval_failures.len(),
            preapproval_failures.join(", ")
        );
    }

    println!("\n=== Onboarding complete! ===");
    Ok(())
}

// ============================================================================
// User service commands
// ============================================================================

pub async fn run_user_service(
    config: BaseConfig,
    command: UserServiceCommands,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        UserServiceCommands::Request {
            reference_id,
            party_name,
        } => {
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Request UserService",
                    &format!("party: {}", config.party_id),
                )
                .await?;
            }
            let expectation = OperationExpectation::RequestUserService {
                party: config.party_id.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestUserService as i32,
                        params: Some(Params::RequestUserService(RequestUserServiceParams {
                            reference_id,
                            party_name,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            println!("UserService requested: {}", result.update_id);
            if let Some(cid) = result.contract_id {
                println!("UserServiceRequest contract: {}", cid);
            }
        }
    }

    Ok(())
}

/// Generates or validates the secp256k1 quote key. Needs no agent identity.
pub fn run_atomic_keygen(quote_override: Option<&str>) -> Result<()> {
    // Never writes a file: validates the configured key or prints material for .env.
    match resolve_quote_key(quote_override) {
        Ok((kf, source)) => {
            let verb = if source.starts_with("--") { "is valid" } else { "is set and valid" };
            println!("{source} {verb}.");
            println!("SPKI public key: {}", kf.pub_spki_hex);
        }
        Err(e) if quote_override.is_some() => return Err(e),
        Err(_) => {
            let kf = atomic_quote::gen_keypair()?;
            println!("Generated a new secp256k1 quote keypair (NOT persisted).");
            println!("Add this line to the agent's .env:");
            println!();
            println!("ATOMIC_QUOTE_PRIVATE_KEY={}", kf.priv_scalar_hex);
            println!();
            println!("SPKI public key: {}", kf.pub_spki_hex);
            println!(
                "⚠ Keep the .env safe: this key signs all quotes for venues created with it."
            );
        }
    }
    Ok(())
}

/// Shared by the identity-bound and anonymous `info network` paths.
fn print_network_info(rates: &orderbook_proto::ledger::GetDsoRatesResponse) {

    println!("\n=== Network Info ===\n");
    println!("DSO Party:             {}", rates.dso_party_id);
    println!("Current Round:         {}", rates.current_round);
    println!("CC/USD Rate:           {}", rates.cc_usd_rate);
    if !rates.featured_app_issuance.is_empty() {
        println!("Featured App Issuance: {}", rates.featured_app_issuance);
    }

    println!("\n=== Open Mining Rounds ===\n");
    if rates.open_mining_rounds.is_empty() {
        println!("No open mining rounds found.");
    } else {
        for round in &rates.open_mining_rounds {
            println!("Round {}:", round.round_number);
            println!("  Amulet Price:    {}", round.amulet_price);
            println!("  Opens At:        {}", round.opens_at);
            println!("  Target Closes:   {}", round.target_closes_at);
            println!("  Issuing For:     {}", round.issuing_for);
            println!("  Tick Duration:   {}", round.tick_duration);
            if !round.transfer_config_usd.is_empty() {
                println!("  Transfer Config: {}", round.transfer_config_usd);
            }
            if !round.issuance_config.is_empty() {
                println!("  Issuance Config: {}", round.issuance_config);
            }
            println!();
        }
    }

    println!("=== Issuing Mining Rounds ===\n");
    if rates.issuing_mining_rounds.is_empty() {
        println!("No issuing mining rounds found.");
    } else {
        for round in &rates.issuing_mining_rounds {
            println!("Round {}:", round.round_number);
            println!(
                "  Featured App:     {}",
                round.issuance_per_featured_app_reward_coupon
            );
            println!(
                "  Unfeatured App:   {}",
                round.issuance_per_unfeatured_app_reward_coupon
            );
            println!(
                "  Validator:        {}",
                round.issuance_per_validator_reward_coupon
            );
            println!(
                "  SV:               {}",
                round.issuance_per_sv_reward_coupon
            );
            if let Some(faucet) = &round.opt_issuance_per_validator_faucet_coupon {
                println!("  Validator Faucet: {}", faucet);
            }
            println!("  Opens At:         {}", round.opens_at);
            println!("  Target Closes:    {}", round.target_closes_at);
            println!();
        }
    }
}

/// `info network` without an agent identity: the RPC relays this query
/// unauthenticated, so no token is sent.
pub async fn run_info_network_anonymous(
    grpc_url: &str,
    connection_timeout_secs: u64,
    request_timeout_secs: u64,
) -> Result<()> {
    let mut client = ledger_client::DAppProviderClient::new_anonymous(
        grpc_url,
        Some(connection_timeout_secs),
        Some(request_timeout_secs),
    )
    .await?;
    print_network_info(&client.get_dso_rates().await?);
    Ok(())
}

/// `info party` without a private key: the identity is read from the
/// environment, and the public key derived only when a key is configured.
pub fn run_info_party_from_env(party_override: Option<&str>, key: Option<&str>) -> Result<()> {
    let party = party_override
        .map(str::to_string)
        .or_else(|| std::env::var("PARTY_AGENT").ok())
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("PARTY_AGENT env var (or --party) is required"))?;
    println!("Party ID: {}", party);

    let configured_key = key
        .map(str::to_string)
        .or_else(|| std::env::var("PARTY_AGENT_PRIVATE_KEY").ok())
        .filter(|k| !k.trim().is_empty());
    let public_key_hex = match configured_key {
        Some(k) => {
            let bytes = agent_logic::config::decode_private_key(&k)?;
            Some(agent_logic::auth::get_public_key_hex(&bytes))
        }
        None => std::env::var("PARTY_AGENT_PUBLIC_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .map(|b58| agent_logic::config::decode_public_key(&b58).map(hex::encode))
            .transpose()?,
    };
    match public_key_hex {
        Some(hex) => println!("Public key: {}", hex),
        None => println!("Public key: (no key configured)"),
    }
    if let Ok(node) = std::env::var("NODE_NAME") {
        println!("Node name: {}", node);
    }
    Ok(())
}

/// `sign` with an explicit key: signing needs no party and no agent config.
pub fn run_sign_standalone(command: SignCommands, private_key: &str) -> Result<()> {
    let key = agent_logic::sign::resolve_signing_key(&[0u8; 32], Some(private_key))?;
    match command {
        SignCommands::Multihash { input, .. } => {
            println!("{}", agent_logic::sign::sign_multihash(&key, &input)?)
        }
        SignCommands::Message { input, .. } => {
            println!("{}", agent_logic::sign::sign_message(&key, &input))
        }
        SignCommands::Binary { input, .. } => {
            println!("{}", agent_logic::sign::sign_binary(&key, &input)?)
        }
    }
    Ok(())
}

pub fn run_sign(config: BaseConfig, command: SignCommands) -> Result<()> {
    match command {
        SignCommands::Multihash { input, private_key } => {
            let key = agent_logic::sign::resolve_signing_key(
                &config.private_key.expose(),
                private_key.as_deref(),
            )?;
            println!("{}", agent_logic::sign::sign_multihash(&key, &input)?);
        }
        SignCommands::Message { input, private_key } => {
            let key = agent_logic::sign::resolve_signing_key(
                &config.private_key.expose(),
                private_key.as_deref(),
            )?;
            println!("{}", agent_logic::sign::sign_message(&key, &input));
        }
        SignCommands::Binary { input, private_key } => {
            let key = agent_logic::sign::resolve_signing_key(
                &config.private_key.expose(),
                private_key.as_deref(),
            )?;
            println!("{}", agent_logic::sign::sign_binary(&key, &input)?);
        }
    }
    Ok(())
}

// ============================================================================
// Faucet commands
// ============================================================================

pub async fn run_faucet(config: BaseConfig, command: FaucetCommands, verbose: bool) -> Result<()> {
    let mut client = ledger_client::DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        FaucetCommands::Get {
            token,
            admin,
            ticket,
            amount,
            dry_run,
        } => {
            // "CC" and "Amulet" (case-insensitive) both mean Canton Coin — normalize
            // to "Amulet" before signing, since the server canonical includes token_name.
            let is_cc = token.eq_ignore_ascii_case("cc") || token.eq_ignore_ascii_case("amulet");
            let normalized_token = if is_cc { "Amulet".to_string() } else { token };

            let resolved_admin = match admin {
                Some(a) => a,
                None if is_cc => config.dso_party.clone(),
                None => {
                    let (_, registry) = config.resolve_instrument(&normalized_token);
                    if registry.is_empty() {
                        anyhow::bail!(
                            "--admin not provided and token '{}' was not found in the \
                             instrument registry fetched from orderbook-rpc. Pass \
                             --admin explicitly, or check that the token is onboarded \
                             (see `cloud-agent info instruments`).",
                            normalized_token
                        );
                    }
                    registry
                }
            };

            let response = client
                .request_faucet(FaucetRequest {
                    token_name: normalized_token,
                    token_admin: resolved_admin,
                    ticket,
                    amount: amount.unwrap_or_default(),
                    dry_run,
                    request_signature: None,
                })
                .await?;

            if response.success {
                if response.is_dry_run {
                    println!("Faucet dry run approved:");
                } else {
                    println!("Faucet transfer completed:");
                    println!("  Update ID: {}", response.update_id);
                }
                println!(
                    "  Token:     {} (admin: {})",
                    response.token_name, response.token_admin
                );
                println!("  Amount:    {}", response.amount_approved);
            } else {
                println!(
                    "Faucet request failed: {}",
                    response.error_message.as_deref().unwrap_or("unknown")
                );
            }

            if verbose {
                println!("\nFull response: {:?}", response);
            }
        }
    }

    Ok(())
}

// ============================================================================
// Lock commands
// ============================================================================

pub async fn run_lock(
    config: BaseConfig,
    command: LockCommands,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
) -> Result<()> {
    let mut client = ledger_client::DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        LockCommands::Holdings {
            lock_service_cid,
            lock_service_template_id,
            lock_service_blob,
            amount,
            instrument_id,
            context,
            allocations_file,
            fee_parties,
            fee_amounts,
            amulet_cids,
        } => {
            let context = context.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

            // Load allocations from file if provided
            let allocations: Vec<ProtoVotingAllocation> = match allocations_file {
                Some(path) => {
                    let content = std::fs::read_to_string(&path)
                        .with_context(|| format!("Failed to read allocations file '{}'", path))?;
                    let allocs: Vec<serde_json::Value> = serde_json::from_str(&content)
                        .with_context(|| format!("Invalid allocations JSON in '{}'", path))?;
                    allocs
                        .iter()
                        .map(|a| ProtoVotingAllocation {
                            context: a
                                .get("context")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            amount: a
                                .get("amount")
                                .and_then(|v| v.as_str())
                                .unwrap_or("0")
                                .to_string(),
                        })
                        .collect()
                }
                None => vec![],
            };

            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Lock Holdings",
                    &format!(
                        "amount: {}, instrument: {}, context: {}",
                        amount, instrument_id, context
                    ),
                )
                .await?;
            }

            let expectation = OperationExpectation::LockHoldings {
                party: config.party_id.clone(),
                amount: amount.clone(),
                instrument_id: instrument_id.clone(),
                context: context.clone(),
            };

            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::LockHoldings as i32,
                        params: Some(Params::LockHoldings(LockHoldingsParams {
                            lock_service_cid,
                            lock_service_template_id,
                            lock_service_blob,
                            amount,
                            instrument_id,
                            context: context.clone(),
                            allocations,
                            fee_party_ids: fee_parties,
                            fee_amounts,
                            amulet_cids,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;

            println!("Lock holdings submitted, update id: {}", result.update_id);
            println!("  Context: {}", context);
        }
        LockCommands::Vote {
            lock_controller_cid,
            lock_controller_template_id,
            requests_file,
            fee_parties,
            fee_amounts,
            amulet_cids,
        } => {
            let content = std::fs::read_to_string(&requests_file)
                .with_context(|| format!("Failed to read requests file '{}'", requests_file))?;
            let reqs_json: Vec<serde_json::Value> = serde_json::from_str(&content)
                .with_context(|| format!("Invalid requests JSON in '{}'", requests_file))?;
            let requests: Vec<ProtoVotingRequest> = reqs_json
                .iter()
                .map(|r| ProtoVotingRequest {
                    context: r
                        .get("context")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    amount: r
                        .get("amount")
                        .and_then(|v| v.as_str())
                        .unwrap_or("0")
                        .to_string(),
                    direction: r
                        .get("direction")
                        .and_then(|v| v.as_str())
                        .unwrap_or("VoteLock")
                        .to_string(),
                })
                .collect();

            let expectation = OperationExpectation::ProcessLockUnlockRequests {
                party: config.party_id.clone(),
                request_count: requests.len(),
            };

            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::ProcessLockUnlockRequests as i32,
                        params: Some(Params::ProcessLockUnlockRequests(
                            ProcessLockUnlockRequestsParams {
                                lock_controller_cid,
                                lock_controller_template_id,
                                requests,
                                fee_party_ids: fee_parties,
                                fee_amounts,
                                amulet_cids,
                            },
                        )),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;

            println!("Vote submitted, update id: {}", result.update_id);
        }
        LockCommands::Resize {
            lock_controller_cid,
            lock_controller_template_id,
            new_amount,
            instrument_id,
            requests_file,
            fee_parties,
            fee_amounts,
            amulet_cids,
        } => {
            let requests: Vec<ProtoVotingRequest> = match requests_file {
                Some(path) => {
                    let content = std::fs::read_to_string(&path)
                        .with_context(|| format!("Failed to read requests file '{}'", path))?;
                    let reqs_json: Vec<serde_json::Value> = serde_json::from_str(&content)
                        .with_context(|| format!("Invalid requests JSON in '{}'", path))?;
                    reqs_json
                        .iter()
                        .map(|r| ProtoVotingRequest {
                            context: r
                                .get("context")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            amount: r
                                .get("amount")
                                .and_then(|v| v.as_str())
                                .unwrap_or("0")
                                .to_string(),
                            direction: r
                                .get("direction")
                                .and_then(|v| v.as_str())
                                .unwrap_or("VoteLock")
                                .to_string(),
                        })
                        .collect()
                }
                None => vec![],
            };

            let expectation = OperationExpectation::ResizeLock {
                party: config.party_id.clone(),
                new_amount: new_amount.clone(),
            };

            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::ResizeLock as i32,
                        params: Some(Params::ResizeLock(ResizeLockParams {
                            lock_controller_cid,
                            lock_controller_template_id,
                            new_amount,
                            requests,
                            instrument_id,
                            fee_party_ids: fee_parties,
                            fee_amounts,
                            amulet_cids,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;

            println!("Resize lock submitted, update id: {}", result.update_id);
        }
        LockCommands::Terminate {
            lock_controller_cid,
            lock_controller_template_id,
            fee_parties,
            fee_amounts,
            amulet_cids,
        } => {
            let expectation = OperationExpectation::TerminateLock {
                party: config.party_id.clone(),
            };

            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::TerminateLock as i32,
                        params: Some(Params::TerminateLock(TerminateLockParams {
                            lock_controller_cid,
                            lock_controller_template_id,
                            fee_party_ids: fee_parties,
                            fee_amounts,
                            amulet_cids,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;

            println!("Terminate lock submitted, update id: {}", result.update_id);
        }
    }

    Ok(())
}

// ============================================================================
// Atomic (RFQ V2 / AtomicDVP) commands
// ============================================================================

/// An on-ledger AtomicDVP venue owned by this LP (CLI view; also used by
/// library embedders via [`atomic_find_venues`]).
pub struct AtomicVenueSummary {
    pub contract_id: String,
    pub pair_name: String,
    pub provider: String,
    pub base_admin: String,
    pub base_id: String,
    pub quote_admin: String,
    pub quote_id: String,
    pub quote_public_key: String,
}

/// One LIQUIDITY-heartbeat-style summary line per instrument for a set of
/// parsed ACS holdings — `{sym}: {total} bal | holdings N (0 rsvd) <10:…` —
/// shared by `atomic status` and library embedders (e.g. rfqv2-test status).
/// USD prices: USDC* = $1, CC = the caller-provided DSO rate, anything else
/// the `{sym}-USDCx|USDC` market mid (bid/ask mid, else last) fetched via a
/// lazily-built OrderbookClient. `rsvd` is a runtime-cache concept — offline
/// ACS reads always show 0.
pub async fn holdings_status_lines(
    config: &BaseConfig,
    holdings: &[holdings_cache::CachedHolding],
    cc_usd_rate: Option<f64>,
) -> Vec<String> {
    use std::collections::BTreeMap;
    let mut by_instrument: BTreeMap<String, Vec<rust_decimal::Decimal>> = BTreeMap::new();
    for h in holdings {
        by_instrument
            .entry(h.instrument.clone())
            .or_default()
            .push(h.amount);
    }

    let mut price_client: Option<agent_logic::client::OrderbookClient> = None;
    let mut lines = Vec::with_capacity(by_instrument.len());
    for (key, amounts) in &by_instrument {
        // The cache key is "admin::<on-chain wire id>"; the CC key stays "CC".
        // Wire id == internal id for legacy tokens, but issuer-minted tokens
        // can carry an opaque UUID on-chain — translate for display,
        // canonicity and price lookups, which all speak internal ids.
        let sym = key.rsplit("::").next().unwrap_or(key).to_string();
        let internal = config
            .internal_id_for_wire(&sym)
            .unwrap_or_else(|| sym.clone());
        // Canonicity: instruments can be DUPLICATED under different admins
        // (e.g. two cETH issuers on devnet). Only the admin the instruments
        // table lists (config.instrument_registries) is tradable — settle and
        // split resolve through it. Foreign-admin holdings are shown but
        // unmistakably labeled so a status never contradicts the split path.
        let canonical = key == holdings_cache::CC_INSTRUMENT
            || config
                .instrument_registries
                .get(&internal)
                .is_some_and(|registry| key == &holdings_cache::instrument_key(registry, &sym));
        let usd_price = if internal.starts_with("USDC") {
            Some(1.0)
        } else if key == holdings_cache::CC_INSTRUMENT {
            cc_usd_rate
        } else {
            if price_client.is_none() {
                price_client = agent_logic::client::OrderbookClient::new(config).await.ok();
            }
            let mut found = None;
            if let Some(client) = price_client.as_mut() {
                for stable in ["USDCx", "USDC"] {
                    if let Ok(resp) = client.get_price(&format!("{internal}-{stable}")).await {
                        let mid = match (resp.bid, resp.ask) {
                            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
                            _ => resp.last,
                        };
                        if mid > 0.0 {
                            found = Some(mid);
                            break;
                        }
                    }
                }
            }
            found
        };
        let pairs: Vec<(rust_decimal::Decimal, bool)> =
            amounts.iter().map(|a| (*a, false)).collect();
        let h = backend::bucket_by_usd(amounts.len(), 0, &pairs, usd_price);
        let total: rust_decimal::Decimal = amounts.iter().sum();
        let histogram_str = if h.priced {
            format!(
                "<10:{} 10-20:{} 20-50:{} 50-100:{} >100:{}",
                h.under_10, h.b10_20, h.b20_50, h.b50_100, h.over_100
            )
        } else {
            "[no USD price]".to_string()
        };
        let label = if canonical {
            internal.clone()
        } else {
            let admin = key.strip_suffix(&format!("::{sym}")).unwrap_or(key);
            let admin_prefix: String = admin.chars().take(24).collect();
            format!(
                "{sym} [FOREIGN ADMIN {admin_prefix}… — not in instruments, ignored by trading]"
            )
        };
        lines.push(format!(
            "{}: {} bal | holdings {} ({} rsvd) {}",
            label,
            total.normalize(),
            h.total,
            h.reserved,
            histogram_str
        ));
    }
    lines
}

/// This LP's venues from the ACS (via `GetAtomicContracts`).
pub async fn atomic_find_venues(
    client: &mut AtomicProviderClient,
    party_id: &str,
) -> Result<Vec<AtomicVenueSummary>> {
    let resp = client
        .get_atomic_contracts(&[venue_registry::TEMPLATE_ATOMIC_DVP.to_string()], &[])
        .await?;
    let mut venues = Vec::new();
    for c in resp.contracts {
        if !c.template_id.contains("AtomicDVP:AtomicDVP") {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&c.payload_json) else {
            continue;
        };
        let s = |ptr: &str| {
            payload
                .pointer(ptr)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        if s("/lp").as_deref() != Some(party_id) {
            continue;
        }
        venues.push(AtomicVenueSummary {
            contract_id: c.contract_id,
            pair_name: s("/pairName").unwrap_or_default(),
            provider: s("/provider").unwrap_or_default(),
            base_admin: s("/baseInstrumentId/admin").unwrap_or_default(),
            base_id: s("/baseInstrumentId/id").unwrap_or_default(),
            quote_admin: s("/quoteInstrumentId/admin").unwrap_or_default(),
            quote_id: s("/quoteInstrumentId/id").unwrap_or_default(),
            quote_public_key: s("/quotePublicKey").unwrap_or_default(),
        });
    }
    Ok(venues)
}

/// The LP's secp256k1 quote-signing key and its source label:
/// `--quote-private-key`, else ATOMIC_QUOTE_PRIVATE_KEY (raw 32-byte scalar hex).
fn resolve_quote_key(
    cli_override: Option<&str>,
) -> Result<(atomic_quote::QuoteKeyFile, &'static str)> {
    use agent_logic::secret::Zeroizing;
    let (scalar, source) = match cli_override {
        Some(s) => (Zeroizing::new(s.to_string()), "--quote-private-key"),
        None => (
            Zeroizing::new(
                std::env::var("ATOMIC_QUOTE_PRIVATE_KEY")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow!(
                            "ATOMIC_QUOTE_PRIVATE_KEY is not set — run `atomic keygen` and add the \
                             printed line to .env, or pass --quote-private-key"
                        )
                    })?,
            ),
            "ATOMIC_QUOTE_PRIVATE_KEY",
        ),
    };
    let kf = atomic_quote::keyfile_from_scalar(scalar.trim())
        .with_context(|| format!("{source} is not a valid secp256k1 scalar"))?;
    Ok((kf, source))
}

/// Read + validate the AtomicDVPService disclosure file the provider exported
/// (`orderbook atomic export --file …`). The singleton is sole-signatory
/// provider (fa-design G2), so it is INVISIBLE in the LP's ACS — this file is
/// how the provider distributes the static blob (fa-design §3.1). Returns the
/// contract id. The ledger-service independently discovers + discloses the
/// service at prepare time; this file is the LP-side reference copy.
fn read_service_file(path: &str) -> Result<String> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!(
            "cannot read the AtomicDVPService disclosure file {path} — ask the \
             provider to export it: `orderbook atomic export --file {path}`"
        )
    })?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("{path} is not valid JSON"))?;
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let (cid, tid, blob) = (s("contractId"), s("templateId"), s("createdEventBlob"));
    if cid.is_empty() || blob.is_empty() || !tid.contains(":AtomicDVPService") {
        anyhow::bail!(
            "{path} is not an AtomicDVPService disclosure (need contractId, \
             createdEventBlob, templateId=…:AtomicDVPService) — re-export it: \
             `orderbook atomic export --file {path}`"
        );
    }
    Ok(cid)
}

/// Contract ids of this LP's live SettlementTickets.
pub async fn atomic_list_live_tickets(
    client: &mut AtomicProviderClient,
    party_id: &str,
) -> Result<Vec<String>> {
    let resp = client
        .get_atomic_contracts(
            &[venue_registry::TEMPLATE_SETTLEMENT_TICKET.to_string()],
            &[],
        )
        .await?;
    let mut cids = Vec::new();
    for c in resp.contracts {
        if !c.template_id.contains("AtomicDVP:SettlementTicket") {
            continue;
        }
        let lp = serde_json::from_str::<serde_json::Value>(&c.payload_json)
            .ok()
            .and_then(|v| v.get("lp").and_then(|l| l.as_str()).map(str::to_string));
        if lp.is_some() && lp.as_deref() != Some(party_id) {
            continue;
        }
        cids.push(c.contract_id);
    }
    Ok(cids)
}

/// Resolve a "BASE-QUOTE" market id to on-chain (id, admin) pairs.
pub fn atomic_resolve_market_pair(
    config: &BaseConfig,
    market_id: &str,
) -> Result<((String, String), (String, String))> {
    let (base, quote) = market_id
        .split_once('-')
        .ok_or_else(|| anyhow!("market id '{}' must have the form BASE-QUOTE", market_id))?;
    let (base_id, base_admin) = config.resolve_instrument(base);
    let (quote_id, quote_admin) = config.resolve_instrument(quote);
    if base_admin.is_empty() || quote_admin.is_empty() {
        return Err(anyhow!(
            "cannot resolve instrument admins for market {} (base {}: '{}', quote {}: '{}') — \
             instrument registry not populated",
            market_id,
            base,
            base_admin,
            quote,
            quote_admin
        ));
    }
    Ok(((base_id, base_admin), (quote_id, quote_admin)))
}

/// Split own holdings of one instrument into denomination rungs.
/// `only_deficit`: only create what's missing per rung `[denom, 2*denom)`
/// (setup mode); otherwise split exactly the requested rungs. CC uses the v1
/// `SplitCc` operation; utility instruments go through
/// `TicketService_SplitHoldings`. Returns the update id, or None when there
/// was nothing to do.
#[allow(clippy::too_many_arguments)]
/// Query, filter, and descending-sort the party's holdings for one instrument.
async fn fetch_split_holdings(
    v1_client: &mut DAppProviderClient,
    config: &BaseConfig,
    instr_key: &str,
) -> Result<Vec<holdings_cache::CachedHolding>> {
    let contracts = v1_client
        .get_active_contracts(&[
            holdings_cache::TEMPLATE_AMULET.to_string(),
            holdings_cache::TEMPLATE_HOLDING.to_string(),
        ])
        .await?;
    let mut holdings: Vec<holdings_cache::CachedHolding> =
        acs_worker::parse_acs_holdings(contracts, &config.party_id)
            .into_iter()
            .filter(|h| h.instrument == instr_key)
            .collect();
    holdings.sort_by(|a, b| b.amount.cmp(&a.amount)); // descending
    Ok(holdings)
}

/// Re-fetch holdings after a committed split until none of the inputs that tx
/// spent is still visible — a snapshot that still shows them predates the
/// commit, and submitting from it would reference already-spent contracts.
/// Returns the freshest snapshot and whether it is clean.
async fn refetch_after_split(
    v1_client: &mut DAppProviderClient,
    config: &BaseConfig,
    instr_key: &str,
    spent_cids: &[String],
) -> Result<(Vec<holdings_cache::CachedHolding>, bool)> {
    const ATTEMPTS: u32 = 5;
    const DELAY: std::time::Duration = std::time::Duration::from_secs(2);
    let is_stale = |holdings: &[holdings_cache::CachedHolding]| {
        holdings.iter().any(|h| spent_cids.contains(&h.contract_id))
    };
    let mut holdings = fetch_split_holdings(v1_client, config, instr_key).await?;
    for _ in 1..ATTEMPTS {
        if !is_stale(&holdings) {
            return Ok((holdings, true));
        }
        tokio::time::sleep(DELAY).await;
        holdings = fetch_split_holdings(v1_client, config, instr_key).await?;
    }
    let clean = !is_stale(&holdings);
    Ok((holdings, clean))
}

pub async fn atomic_split_denominations(
    config: &BaseConfig,
    on_chain_id: &str,
    admin: &str,
    rungs: &[(rust_decimal::Decimal, u32)],
    only_deficit: bool,
    verbose: bool,
    dry_run: bool,
    force: bool,
) -> Result<Option<String>> {
    use orderbook_proto::rfqv2::{
        PrepareAtomicTransactionRequest, SplitHoldingsParams, SplitSpec,
        prepare_atomic_transaction_request::Params as AtomicParams,
    };
    use rust_decimal::Decimal;

    let is_cc = on_chain_id == "Amulet";
    let instr_key = if is_cc {
        holdings_cache::CC_INSTRUMENT.to_string()
    } else {
        holdings_cache::instrument_key(admin, on_chain_id)
    };

    let mut v1_client = atomic_swap::create_v1_client(config).await?;
    let mut atomic_client: Option<AtomicProviderClient> = None;
    let mut holdings = fetch_split_holdings(&mut v1_client, config, &instr_key).await?;

    // Full-ladder tracker for the non-deficit (manual `atomic split`) mode:
    // between chunks it must NOT recompute deficits from the ACS — earlier
    // chunks and pre-existing holdings would be double-counted — so the
    // submitted chunk is subtracted instead.
    let mut remaining: Vec<(Decimal, u32)> = rungs.to_vec();

    let initial_want: Vec<(Decimal, u32)> = if only_deficit {
        let amounts: Vec<Decimal> = holdings.iter().map(|h| h.amount).collect();
        split_worker::rung_deficits(rungs, &amounts)
    } else {
        remaining.clone()
    };
    if initial_want.is_empty() {
        return Ok(None);
    }
    let total_outputs: u32 = initial_want.iter().map(|(_, c)| *c).sum();
    // Both split mechanisms are capped per transaction (Amulet rejects >100
    // outputs in one AmuletRules_Transfer with maximum-outputs-exceeded; the
    // CIP-56 SplitHoldings path is chunked identically), so an over-cap ladder
    // is filled across several sequential transactions: submit a capped chunk,
    // re-check, submit the next.
    let max_iters = total_outputs.div_ceil(split_worker::MAX_SPLIT_OUTPUTS_PER_TX) + 2;
    let multi_tx = total_outputs > split_worker::MAX_SPLIT_OUTPUTS_PER_TX;

    let mut last_update: Option<String> = None;
    let mut spent_inputs: Vec<String> = Vec::new();
    let mut committed_txs: u32 = 0;
    let mut prev_outstanding: Option<u32> = None;
    let mut ladder_covered = false;

    for iter in 0..max_iters {
        if iter > 0 {
            let (fresh, clean) =
                refetch_after_split(&mut v1_client, config, &instr_key, &spent_inputs).await?;
            holdings = fresh;
            if !clean {
                tracing::warn!(
                    "{}: ACS still shows inputs spent by the previous split tx — stopping; \
                     the remaining rungs fill via the split worker or the next setup run",
                    instr_key
                );
                break;
            }
        }

        let want: Vec<(Decimal, u32)> = if only_deficit {
            let amounts: Vec<Decimal> = holdings.iter().map(|h| h.amount).collect();
            split_worker::rung_deficits(rungs, &amounts)
        } else {
            remaining.clone()
        };
        let outstanding: u32 = want.iter().map(|(_, c)| *c).sum();
        if outstanding == 0 {
            ladder_covered = true;
            break;
        }
        if only_deficit {
            if let Some(prev) = prev_outstanding {
                if outstanding >= prev {
                    tracing::warn!(
                        "{}: split made no progress ({} output(s) outstanding, was {}) — stopping",
                        instr_key,
                        outstanding,
                        prev
                    );
                    break;
                }
            }
            prev_outstanding = Some(outstanding);
        }

        // Cap the requested rungs to what the balance actually covers (partial
        // fill). All-or-nothing here meant a party holding 6 rungs' worth of
        // funds got NO rungs at all ("insufficient holdings for splits"). Walk
        // the rungs in ladder order taking whole denominations while the budget
        // lasts; the shortfall is warned about and picked up by a later
        // setup/maintenance pass once the party is funded.
        let affordable = {
            use rust_decimal::prelude::ToPrimitive;
            let available: Decimal = holdings.iter().map(|h| h.amount).sum();
            let mut budget = if is_cc {
                // Reciprocal of the CC fee margin applied below (×1.02 + 1).
                ((available - Decimal::ONE) / Decimal::new(102, 2)).max(Decimal::ZERO)
            } else {
                available
            };
            let requested: Decimal = want.iter().map(|(d, c)| *d * Decimal::from(*c)).sum();
            let mut capped: Vec<(Decimal, u32)> = Vec::new();
            for (denom, count) in &want {
                if *denom <= Decimal::ZERO {
                    continue;
                }
                let fits = (budget / *denom).floor().to_u32().unwrap_or(0).min(*count);
                if fits > 0 {
                    capped.push((*denom, fits));
                    budget -= *denom * Decimal::from(fits);
                }
            }
            if capped.is_empty() {
                if committed_txs == 0 {
                    return Err(anyhow!(
                        "insufficient {} holdings for splits: have {}, need {}",
                        instr_key,
                        available,
                        requested
                    ));
                }
                tracing::warn!(
                    "{}: balance exhausted with {} output(s) outstanding — stopping after {} committed split tx(s)",
                    instr_key,
                    outstanding,
                    committed_txs
                );
                break;
            }
            let capped_total: Decimal = capped.iter().map(|(d, c)| *d * Decimal::from(*c)).sum();
            if capped_total < requested {
                tracing::warn!(
                    "{}: partial split — balance {} covers {} of the requested {} ladder total",
                    instr_key,
                    available,
                    capped_total,
                    requested
                );
            }
            capped
        };

        // Never exceed the per-tx output cap; the remainder goes in the next
        // chunk once the change from this tx is back in the ACS.
        let chunk =
            split_worker::cap_split_outputs(&affordable, split_worker::MAX_SPLIT_OUTPUTS_PER_TX);
        let chunk_outputs: u32 = chunk.iter().map(|(_, c)| *c).sum();

        let mut total: Decimal = chunk.iter().map(|(d, c)| *d * Decimal::from(*c)).sum();
        if is_cc {
            // CC fee margin (holding-fee decay + transfer fees)
            total = total * Decimal::new(102, 2) + Decimal::ONE;
        }

        // Largest-first input selection until the total is covered (cap 100).
        let mut input_cids: Vec<String> = Vec::new();
        let mut covered = Decimal::ZERO;
        for h in &holdings {
            if input_cids.len() >= 100 || covered >= total {
                break;
            }
            input_cids.push(h.contract_id.clone());
            covered += h.amount;
        }
        if covered < total {
            if committed_txs == 0 {
                return Err(anyhow!(
                    "insufficient {} holdings for splits: have {}, need {}",
                    instr_key,
                    covered,
                    total
                ));
            }
            tracing::warn!(
                "{}: inputs cover {} of the {} needed for the next chunk — stopping after {} committed split tx(s)",
                instr_key,
                covered,
                total,
                committed_txs
            );
            break;
        }

        let update_id = if is_cc {
            let mut output_amounts: Vec<String> = Vec::new();
            for (denom, count) in &chunk {
                for _ in 0..*count {
                    output_amounts.push(denom.to_string());
                }
            }
            let expectation = OperationExpectation::SplitCc {
                party: config.party_id.clone(),
                output_amounts: output_amounts.clone(),
            };
            v1_client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::SplitCc as i32,
                        params: Some(Params::SplitCc(SplitCcParams {
                            output_amounts,
                            amulet_cids: input_cids.clone(),
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await
                .with_context(|| {
                    format!(
                        "split tx {} for {} ({} tx(s) already committed)",
                        iter + 1,
                        instr_key,
                        committed_txs
                    )
                })?
                .update_id
        } else {
            if atomic_client.is_none() {
                atomic_client = Some(atomic_swap::create_atomic_client(config).await?);
            }
            let splits: Vec<SplitSpec> = chunk
                .iter()
                .map(|(denom, count)| SplitSpec {
                    amount: denom.to_string(),
                    count: *count,
                })
                .collect();
            let expectation = OperationExpectation::SplitHoldings {
                lp_party: config.party_id.clone(),
                instrument_id: on_chain_id.to_string(),
                split_count: splits.len(),
                input_cids: input_cids.clone(),
            };
            atomic_client
                .as_mut()
                .expect("atomic client initialized above")
                .submit_atomic_transaction(
                    PrepareAtomicTransactionRequest {
                        params: Some(AtomicParams::SplitHoldings(SplitHoldingsParams {
                            instrument_id: on_chain_id.to_string(),
                            instrument_admin: admin.to_string(),
                            input_holding_cids: input_cids.clone(),
                            splits,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await
                .with_context(|| {
                    format!(
                        "split tx {} for {} ({} tx(s) already committed)",
                        iter + 1,
                        instr_key,
                        committed_txs
                    )
                })?
                .update_id
        };

        if dry_run {
            // Nothing executes in dry-run, so the loop would never converge —
            // show the first capped tx and report what would follow.
            if outstanding > chunk_outputs {
                let rest = outstanding - chunk_outputs;
                println!(
                    "DRY RUN: showed the first {}-output split tx for {}; {} more output(s) would follow in ~{} further tx(s)",
                    chunk_outputs,
                    instr_key,
                    rest,
                    rest.div_ceil(split_worker::MAX_SPLIT_OUTPUTS_PER_TX)
                );
            }
            return Ok(Some(update_id));
        }

        committed_txs += 1;
        last_update = Some(update_id.clone());
        spent_inputs = input_cids;
        if multi_tx {
            println!(
                "Split tx {} for {}: {} output(s) committed (update {}), {} outstanding",
                committed_txs,
                instr_key,
                chunk_outputs,
                update_id,
                outstanding - chunk_outputs
            );
        }
        if !only_deficit {
            remaining = split_worker::subtract_split_chunk(&remaining, &chunk);
            if remaining.is_empty() {
                ladder_covered = true;
                break;
            }
        }
    }

    if !ladder_covered && committed_txs == max_iters {
        tracing::warn!(
            "{}: split iteration bound ({}) reached — remaining rungs fill via the split worker or the next setup run",
            instr_key,
            max_iters
        );
    }
    Ok(last_update)
}

pub async fn run_atomic(
    config: BaseConfig,
    command: AtomicCommands,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    quote_key_override: Option<agent_logic::secret::Zeroizing<String>>,
) -> Result<()> {
    let quote_override: Option<&str> = quote_key_override.as_deref().map(String::as_str);
    use orderbook_proto::rfqv2::{
        CancelTicketsParams, CreateAtomicDvpVenueParams, IssueTicketsParams,
        PrepareAtomicTransactionRequest, RetireVenueParams, UpdateVenueKeyParams,
        prepare_atomic_transaction_request::Params as AtomicParams,
    };

    match command {
        AtomicCommands::Keygen => return run_atomic_keygen(quote_override),
        AtomicCommands::Venue { command } => match command {
            AtomicVenueCommands::Create { market } => {
                let ((base_id, base_admin), (quote_id, quote_admin)) =
                    atomic_resolve_market_pair(&config, &market)?;
                let (kf, _) = resolve_quote_key(quote_override)?;
                let mut client = atomic_swap::create_atomic_client(&config).await?;
                if let Some(existing) = atomic_find_venues(&mut client, &config.party_id)
                    .await?
                    .into_iter()
                    .find(|v| v.pair_name == market)
                {
                    if existing
                        .quote_public_key
                        .eq_ignore_ascii_case(&kf.pub_spki_hex)
                    {
                        println!(
                            "Venue for {} already exists with the current key: {}",
                            market, existing.contract_id
                        );
                        return Ok(());
                    }
                    return Err(anyhow!(
                        "venue for {} already exists ({}) with a DIFFERENT quote key — \
                         use `atomic venue rotate-key --market {}`",
                        market,
                        existing.contract_id,
                        market
                    ));
                }
                let expectation = OperationExpectation::CreateAtomicDvpVenue {
                    lp_party: config.party_id.clone(),
                    pair_name: market.clone(),
                    quote_public_key_spki_hex: kf.pub_spki_hex.clone(),
                };
                let resp = client
                    .submit_atomic_transaction(
                        PrepareAtomicTransactionRequest {
                            params: Some(AtomicParams::CreateAtomicDvpVenue(
                                CreateAtomicDvpVenueParams {
                                    pair_name: market.clone(),
                                    base_instrument_id: base_id,
                                    base_instrument_admin: base_admin,
                                    quote_instrument_id: quote_id,
                                    quote_instrument_admin: quote_admin,
                                    quote_public_key_spki_hex: kf.pub_spki_hex,
                                },
                            )),
                            request_signature: None,
                        },
                        &expectation,
                        verbose,
                        dry_run,
                        force,
                    )
                    .await?;
                println!(
                    "AtomicDVP venue created for {}, update id: {}",
                    market, resp.update_id
                );
            }
            AtomicVenueCommands::List => {
                let mut client = atomic_swap::create_atomic_client(&config).await?;
                let venues = atomic_find_venues(&mut client, &config.party_id).await?;
                let arr: Vec<serde_json::Value> = venues
                    .iter()
                    .map(|v| {
                        serde_json::json!({
                            "contract_id": v.contract_id,
                            "pair_name": v.pair_name,
                            "provider": v.provider,
                            "base_instrument": { "id": v.base_id, "admin": v.base_admin },
                            "quote_instrument": { "id": v.quote_id, "admin": v.quote_admin },
                            "quote_public_key": v.quote_public_key,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "lp_party": config.party_id,
                        "count": arr.len(),
                        "venues": arr,
                    }))?
                );
            }
            AtomicVenueCommands::RotateKey { market } => {
                let (kf, _) = resolve_quote_key(quote_override)?;
                let mut client = atomic_swap::create_atomic_client(&config).await?;
                let venue = atomic_find_venues(&mut client, &config.party_id)
                    .await?
                    .into_iter()
                    .find(|v| v.pair_name == market)
                    .ok_or_else(|| anyhow!("no AtomicDVP venue for market {}", market))?;
                println!("############################################################");
                println!("# WARNING: rotating the quote key ARCHIVES + RECREATES the");
                println!("# venue contract. EVERY live signed envelope dies instantly");
                println!("# (its disclosed venue cid goes stale) and EVERY signature");
                println!("# made with the old key becomes unverifiable. Run this only");
                println!("# with the LP agent drained of in-flight V2 quotes.");
                println!("############################################################");
                println!("Venue: {} (cid {})", venue.pair_name, venue.contract_id);
                if confirm && !dry_run {
                    let lock = agent_logic::confirm::new_confirm_lock();
                    agent_logic::confirm::confirm_transaction(
                        &lock,
                        "Rotate AtomicDVP venue key",
                        &format!("market: {}, venue: {}", market, venue.contract_id),
                    )
                    .await?;
                }
                let expectation = OperationExpectation::UpdateVenueKey {
                    lp_party: config.party_id.clone(),
                    venue_cid: venue.contract_id.clone(),
                    new_key_spki_hex: kf.pub_spki_hex.clone(),
                };
                let resp = client
                    .submit_atomic_transaction(
                        PrepareAtomicTransactionRequest {
                            params: Some(AtomicParams::UpdateVenueKey(UpdateVenueKeyParams {
                                venue_cid: venue.contract_id,
                                new_quote_public_key_spki_hex: kf.pub_spki_hex,
                            })),
                            request_signature: None,
                        },
                        &expectation,
                        verbose,
                        dry_run,
                        force,
                    )
                    .await?;
                println!(
                    "Venue key rotated for {}, update id: {}",
                    market, resp.update_id
                );
                println!("(the venue has a NEW contract id — restart the LP agent to re-validate)");
            }
            AtomicVenueCommands::Retire { market } => {
                let mut client = atomic_swap::create_atomic_client(&config).await?;
                let venue = atomic_find_venues(&mut client, &config.party_id)
                    .await?
                    .into_iter()
                    .find(|v| v.pair_name == market)
                    .ok_or_else(|| anyhow!("no AtomicDVP venue for market {}", market))?;
                println!("############################################################");
                println!("# WARNING: retiring ARCHIVES the venue contract. EVERY live");
                println!("# signed envelope for this pair dies instantly (its disclosed");
                println!("# venue cid goes stale). Retire only with the LP agent drained");
                println!("# of in-flight V2 quotes for {}.", market);
                println!("############################################################");
                println!("Venue: {} (cid {})", venue.pair_name, venue.contract_id);
                if confirm && !dry_run {
                    let lock = agent_logic::confirm::new_confirm_lock();
                    agent_logic::confirm::confirm_transaction(
                        &lock,
                        "Retire AtomicDVP venue",
                        &format!("market: {}, venue: {}", market, venue.contract_id),
                    )
                    .await?;
                }
                let expectation = OperationExpectation::RetireVenue {
                    lp_party: config.party_id.clone(),
                    venue_cid: venue.contract_id.clone(),
                };
                let resp = client
                    .submit_atomic_transaction(
                        PrepareAtomicTransactionRequest {
                            params: Some(AtomicParams::RetireVenue(RetireVenueParams {
                                venue_cid: venue.contract_id,
                            })),
                            request_signature: None,
                        },
                        &expectation,
                        verbose,
                        dry_run,
                        force,
                    )
                    .await?;
                println!(
                    "AtomicDVP venue retired for {}, update id: {}",
                    market, resp.update_id
                );
                println!(
                    "(re-create under the new registrar with `atomic venue create --market {}`)",
                    market
                );
            }
        },

        AtomicCommands::Tickets { command } => {
            let mut client = atomic_swap::create_atomic_client(&config).await?;
            match command {
                AtomicTicketCommands::Issue { count } => {
                    if count == 0 {
                        return Err(anyhow!("--count must be > 0"));
                    }
                    let ticket_ids: Vec<String> = (0..count)
                        .map(|_| uuid::Uuid::now_v7().to_string())
                        .collect();
                    let expectation = OperationExpectation::IssueTickets {
                        lp_party: config.party_id.clone(),
                        ticket_count: ticket_ids.len(),
                    };
                    let resp = client
                        .submit_atomic_transaction(
                            PrepareAtomicTransactionRequest {
                                params: Some(AtomicParams::IssueTickets(IssueTicketsParams {
                                    ticket_ids,
                                })),
                                request_signature: None,
                            },
                            &expectation,
                            verbose,
                            dry_run,
                            force,
                        )
                        .await?;
                    println!("Issued {} tickets, update id: {}", count, resp.update_id);
                }
                AtomicTicketCommands::Cancel { cids, all_free } => {
                    let ticket_cids: Vec<String> = if all_free {
                        atomic_list_live_tickets(&mut client, &config.party_id).await?
                    } else {
                        cids
                    };
                    if ticket_cids.is_empty() {
                        println!("No tickets to cancel");
                        return Ok(());
                    }
                    println!("Cancelling {} ticket(s)", ticket_cids.len());
                    if confirm && !dry_run {
                        let lock = agent_logic::confirm::new_confirm_lock();
                        agent_logic::confirm::confirm_transaction(
                            &lock,
                            "Cancel SettlementTickets",
                            &format!("{} ticket(s)", ticket_cids.len()),
                        )
                        .await?;
                    }
                    let expectation = OperationExpectation::CancelTickets {
                        lp_party: config.party_id.clone(),
                        ticket_count: ticket_cids.len(),
                    };
                    let resp = client
                        .submit_atomic_transaction(
                            PrepareAtomicTransactionRequest {
                                params: Some(AtomicParams::CancelTickets(CancelTicketsParams {
                                    ticket_cids,
                                })),
                                request_signature: None,
                            },
                            &expectation,
                            verbose,
                            dry_run,
                            force,
                        )
                        .await?;
                    println!("Tickets cancelled, update id: {}", resp.update_id);
                }
            }
        }

        AtomicCommands::Split {
            market,
            instrument,
            splits,
        } => {
            let specs: Vec<String> = splits
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let rungs = split_worker::parse_splits(&specs)?;
            let (base_instr, quote_instr) = market
                .split_once('-')
                .ok_or_else(|| anyhow!("market id '{}' must have the form BASE-QUOTE", market))?;
            let target = match instrument.to_ascii_lowercase().as_str() {
                "base" => base_instr,
                "quote" => quote_instr,
                other => {
                    return Err(anyhow!(
                        "--instrument must be 'base' or 'quote', got '{}'",
                        other
                    ));
                }
            };
            let (on_chain_id, admin) = config.resolve_instrument(target);
            if on_chain_id != "Amulet" && admin.is_empty() {
                return Err(anyhow!(
                    "cannot resolve registry admin for instrument '{}' — instrument registry not populated",
                    target
                ));
            }
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Split holdings",
                    &format!(
                        "market: {}, instrument: {} ({}), splits: {}",
                        market, target, on_chain_id, splits
                    ),
                )
                .await?;
            }
            match atomic_split_denominations(
                &config,
                &on_chain_id,
                &admin,
                &rungs,
                false,
                verbose,
                dry_run,
                force,
            )
            .await?
            {
                Some(update_id) => println!("Split submitted, update id: {}", update_id),
                None => println!("Nothing to split"),
            }
        }

        AtomicCommands::Status { market } => {
            let mut client = atomic_swap::create_atomic_client(&config).await?;
            let local_key = resolve_quote_key(quote_override).ok().map(|(kf, _)| kf);

            println!("\n=== AtomicDVP venues ===\n");
            let venues = atomic_find_venues(&mut client, &config.party_id).await?;
            let shown: Vec<&AtomicVenueSummary> = venues
                .iter()
                .filter(|v| market.as_deref().is_none_or(|m| v.pair_name == m))
                .collect();
            if shown.is_empty() {
                println!("(none)");
            }
            for v in shown {
                let key_status = match &local_key {
                    Some(kf) if v.quote_public_key.eq_ignore_ascii_case(&kf.pub_spki_hex) => {
                        "MATCHES local keyfile"
                    }
                    Some(_) => "DOES NOT MATCH local keyfile",
                    None => "no local keyfile to compare",
                };
                println!("{}: {}", v.pair_name, v.contract_id);
                println!("  provider:  {}", v.provider);
                println!("  base:      {} (admin {})", v.base_id, v.base_admin);
                println!("  quote:     {} (admin {})", v.quote_id, v.quote_admin);
                println!("  quote key: {}", key_status);
            }

            match read_service_file("atomic-dvp-service.json") {
                Ok(cid) => println!("\nAtomicDVPService (from atomic-dvp-service.json): {}", cid),
                Err(_) => println!(
                    "\nAtomicDVPService: no atomic-dvp-service.json — get it from the \
                     provider (`orderbook atomic export --file atomic-dvp-service.json`)"
                ),
            }
            let tickets = atomic_list_live_tickets(&mut client, &config.party_id).await?;
            println!("Live tickets:  {}", tickets.len());

            // Per-instrument holdings summary in the LIQUIDITY heartbeat
            // format (one line per instrument — a per-amount dump is huge
            // with 100+ ladder/change holdings).
            let mut v1_client = atomic_swap::create_v1_client(&config).await?;
            let contracts = v1_client
                .get_active_contracts(&[
                    holdings_cache::TEMPLATE_AMULET.to_string(),
                    holdings_cache::TEMPLATE_HOLDING.to_string(),
                ])
                .await?;
            let holdings = acs_worker::parse_acs_holdings(contracts, &config.party_id);
            let dso_rate = v1_client
                .get_dso_rates()
                .await
                .ok()
                .and_then(|r| r.cc_usd_rate.parse::<f64>().ok())
                .filter(|p| *p > 0.0);
            println!("\n=== Holdings (USD-bucketed, LIQUIDITY format) ===\n");
            match holdings_status_lines(&config, &holdings, dso_rate).await {
                lines if lines.is_empty() => println!("(no unlocked holdings)"),
                lines => {
                    for line in lines {
                        println!("{line}");
                    }
                }
            }
        }

        AtomicCommands::Setup { service_file } => {
            println!("=== Atomic (RFQ V2) setup for {} ===", config.party_id);

            // 1. quote key: --quote-private-key, else ATOMIC_QUOTE_PRIVATE_KEY — the
            // same resolution the runtime agent uses
            let (kf, source) = resolve_quote_key(quote_override)?;
            println!("[1/6] Quote key (from {source}): {}", kf.pub_spki_hex);

            let warnings =
                atomic_setup_agent(&config, &kf, Some(&service_file), verbose, dry_run, force)
                    .await?;
            if warnings == 0 {
                println!("Setup complete. Run `atomic status` to verify, then restart the agent.");
            } else {
                println!(
                    "Setup completed with {warnings} warning(s) — review the [x/6] warnings above, \
                     then run `atomic status` to verify."
                );
            }
        }
    }

    Ok(())
}

/// Steps 2-6 of the per-LP RFQ V2 setup — AtomicDVPService reference check,
/// venue per rfq_v2-enabled market, receiving preapprovals, ticket batch,
/// denomination splits — extracted from the `atomic setup` CLI arm so library
/// embedders (e.g. a test harness provisioning many agents in one process)
/// can call it with a programmatically-built `BaseConfig` and a
/// caller-provided quote key (theirs comes from a DB, not
/// ATOMIC_QUOTE_PRIVATE_KEY). `service_file: None` skips the disclosure-file
/// reference check — venue creation is prepared server-side with the server's
/// own disclosure either way.
///
/// Setup stays best-effort (every step warns and continues), but the number of
/// step warnings is returned so callers can report an honest summary instead
/// of an unconditional "Setup complete.".
pub async fn atomic_setup_agent(
    config: &BaseConfig,
    quote_key: &atomic_quote::QuoteKeyFile,
    service_file: Option<&str>,
    verbose: bool,
    dry_run: bool,
    force: bool,
) -> Result<u32> {
    use orderbook_proto::rfqv2::{
        CreateAtomicDvpVenueParams, IssueTicketsParams, PrepareAtomicTransactionRequest,
        prepare_atomic_transaction_request::Params as AtomicParams,
    };
    let mut warnings: u32 = 0;
    {
        // Alias so the body below (moved verbatim from the CLI arm) keeps
        // its original name for the key.
        let kf = quote_key;

        let mut client = atomic_swap::create_atomic_client(config).await?;

        // 2. AtomicDVPService disclosure file check — the singleton is
        // provider-only (invisible in the LP's ACS); the provider exports
        // its blob and the LP keeps a reference copy. Venue creation is
        // prepared server-side with the server's own disclosure.
        match service_file {
            Some(sf) => {
                let service_cid = read_service_file(sf)?;
                println!("[2/6] AtomicDVPService (from {sf}): {service_cid}");
            }
            None => println!(
                "[2/6] AtomicDVPService disclosure file not provided — check skipped \
                     (the ledger-service discloses the singleton at prepare time)"
            ),
        }

        // 3. venue per rfq_v2-enabled market (skip existing with matching key)
        let v2_markets: Vec<&agent_logic::config::MarketConfig> = config
            .markets
            .iter()
            .filter(|m| m.enabled)
            .filter(|m| {
                m.rfq
                    .as_ref()
                    .is_some_and(|r| r.enabled && r.v2.as_ref().is_some_and(|v| v.enabled))
            })
            .collect();
        if v2_markets.is_empty() {
            println!(
                "[3/6] No [markets.rfq.v2]-enabled markets in agent.toml — \
                     skipping venues / preapprovals / splits"
            );
        }
        let venues = atomic_find_venues(&mut client, &config.party_id).await?;
        for m in &v2_markets {
            if let Some(v) = venues.iter().find(|v| v.pair_name == m.market_id) {
                if v.quote_public_key.eq_ignore_ascii_case(&kf.pub_spki_hex) {
                    println!(
                        "[3/6] Venue for {} exists with matching key — skipped",
                        m.market_id
                    );
                } else {
                    warnings += 1;
                    tracing::warn!(
                        "[3/6] Venue for {} exists with a DIFFERENT key ({}) — \
                             run `atomic venue rotate-key --market {}`",
                        m.market_id,
                        v.contract_id,
                        m.market_id
                    );
                }
                continue;
            }
            match atomic_resolve_market_pair(&config, &m.market_id) {
                Ok(((base_id, base_admin), (quote_id, quote_admin))) => {
                    let expectation = OperationExpectation::CreateAtomicDvpVenue {
                        lp_party: config.party_id.clone(),
                        pair_name: m.market_id.clone(),
                        quote_public_key_spki_hex: kf.pub_spki_hex.clone(),
                    };
                    match client
                        .submit_atomic_transaction(
                            PrepareAtomicTransactionRequest {
                                params: Some(AtomicParams::CreateAtomicDvpVenue(
                                    CreateAtomicDvpVenueParams {
                                        pair_name: m.market_id.clone(),
                                        base_instrument_id: base_id,
                                        base_instrument_admin: base_admin,
                                        quote_instrument_id: quote_id,
                                        quote_instrument_admin: quote_admin,
                                        quote_public_key_spki_hex: kf.pub_spki_hex.clone(),
                                    },
                                )),
                                request_signature: None,
                            },
                            &expectation,
                            verbose,
                            dry_run,
                            force,
                        )
                        .await
                    {
                        Ok(resp) => println!(
                            "[3/6] Venue created for {} (update {})",
                            m.market_id, resp.update_id
                        ),
                        Err(e) => {
                            warnings += 1;
                            tracing::warn!(
                                "[3/6] Venue creation for {} failed: {:#}",
                                m.market_id,
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    warnings += 1;
                    tracing::warn!("[3/6] {} skipped: {:#}", m.market_id, e);
                }
            }
        }

        // 4. LP receiving preapprovals per non-CC instrument across those
        // markets (the LP can receive either leg depending on direction)
        let mut v1_client = atomic_swap::create_v1_client(&config).await?;
        let mut instruments: Vec<String> = Vec::new();
        for m in &v2_markets {
            if let Some((base, quote)) = m.market_id.split_once('-') {
                for instr in [base, quote] {
                    if !instruments.iter().any(|i| i == instr) {
                        instruments.push(instr.to_string());
                    }
                }
            }
        }
        for instr in &instruments {
            match atomic_swap::ensure_receiver_preapproval(
                &config,
                &mut v1_client,
                instr,
                verbose,
                dry_run,
                force,
            )
            .await
            {
                Ok(true) => println!("[4/6] Receiving preapproval created for {}", instr),
                Ok(false) => println!(
                    "[4/6] Receiving preapproval for {} already present / not needed",
                    instr
                ),
                Err(e) => {
                    warnings += 1;
                    tracing::warn!("[4/6] Preapproval for {} failed: {:#}", instr, e);
                }
            }
        }

        // 5. ticket batch iff ticket_threshold_usd is configured (D1)
        let v2cfg = config
            .liquidity_provider
            .as_ref()
            .and_then(|lp| lp.rfq_v2.as_ref());
        match v2cfg {
            Some(v2) if v2.ticket_threshold_usd.is_some() => {
                let live = atomic_list_live_tickets(&mut client, &config.party_id)
                    .await?
                    .len();
                if live >= v2.ticket_low_water {
                    println!(
                        "[5/6] {} live tickets (>= low water {}) — issue skipped",
                        live, v2.ticket_low_water
                    );
                } else {
                    let ticket_ids: Vec<String> = (0..v2.ticket_batch_size)
                        .map(|_| uuid::Uuid::now_v7().to_string())
                        .collect();
                    let expectation = OperationExpectation::IssueTickets {
                        lp_party: config.party_id.clone(),
                        ticket_count: ticket_ids.len(),
                    };
                    match client
                        .submit_atomic_transaction(
                            PrepareAtomicTransactionRequest {
                                params: Some(AtomicParams::IssueTickets(IssueTicketsParams {
                                    ticket_ids,
                                })),
                                request_signature: None,
                            },
                            &expectation,
                            verbose,
                            dry_run,
                            force,
                        )
                        .await
                    {
                        Ok(resp) => println!(
                            "[5/6] Issued {} tickets (update {})",
                            v2.ticket_batch_size, resp.update_id
                        ),
                        Err(e) => {
                            warnings += 1;
                            tracing::warn!("[5/6] Ticket issue failed: {:#}", e);
                        }
                    }
                }
            }
            _ => println!(
                "[5/6] ticket_threshold_usd not configured — tickets skipped (ticketless quoting)"
            ),
        }

        // 6. denomination splits per INSTRUMENT (best effort). The global
        // [liquidity_provider.rfq_v2.denominations] ladder map wins; the
        // legacy per-market ladders (applied to both legs, first market
        // wins per instrument) remain as fallback.
        let max_concurrent = config
            .liquidity_provider
            .as_ref()
            .map(|lp| lp.max_concurrent_rfqs)
            .unwrap_or(10);
        let global_denoms = v2cfg.map(|v| v.denominations.clone()).unwrap_or_default();
        let mut instrument_ladders: Vec<(String, Vec<String>)> = Vec::new();
        if !global_denoms.is_empty() {
            for (symbol, ladder) in &global_denoms {
                instrument_ladders.push((symbol.clone(), ladder.clone()));
            }
        } else {
            let mut seen = std::collections::HashSet::new();
            for m in &v2_markets {
                let Some(v2m) = m.rfq.as_ref().and_then(|r| r.v2.as_ref()) else {
                    continue;
                };
                let ladder: Vec<String> = if v2m.denominations.is_empty() {
                    m.base_order_size
                        .as_ref()
                        .and_then(|s| s.parse::<rust_decimal::Decimal>().ok())
                        .map(|amt| vec![format!("{amt}x{max_concurrent}")])
                        .unwrap_or_default()
                } else {
                    v2m.denominations.clone()
                };
                if ladder.is_empty() {
                    continue;
                }
                let Some((base, quote)) = m.market_id.split_once('-') else {
                    continue;
                };
                for instr in [base, quote] {
                    if seen.insert(instr.to_string()) {
                        instrument_ladders.push((instr.to_string(), ladder.clone()));
                    }
                }
            }
        }
        for (symbol, ladder) in instrument_ladders {
            let rungs: Vec<(rust_decimal::Decimal, u32)> = match split_worker::parse_splits(&ladder)
            {
                Ok(r) => r,
                Err(e) => {
                    warnings += 1;
                    tracing::warn!("[6/6] {}: bad denominations: {:#}", symbol, e);
                    continue;
                }
            };
            if rungs.is_empty() {
                continue;
            }
            let (on_chain_id, admin) = config.resolve_instrument(&symbol);
            match atomic_split_denominations(
                &config,
                &on_chain_id,
                &admin,
                &rungs,
                true,
                verbose,
                dry_run,
                force,
            )
            .await
            {
                Ok(Some(update_id)) => println!(
                    "[6/6] Split committed for {} (update {})",
                    symbol, update_id
                ),
                Ok(None) => println!("[6/6] Denomination coverage OK for {}", symbol),
                Err(e) => {
                    warnings += 1;
                    tracing::warn!("[6/6] Split for {} failed: {:#}", symbol, e);
                }
            }
        }
    }

    Ok(warnings)
}

#[cfg(test)]
mod onboard_key_tests {
    use super::*;
    use agent_logic::secret::Zeroizing;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn scratch_env(content: Option<&str>) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cloud-agent-onboard-key-{}-{n}.env",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        if let Some(c) = content {
            std::fs::write(&path, c).unwrap();
        }
        path
    }

    fn keypair() -> (Zeroizing<String>, String) {
        let (priv_b58, pub_b58) = agent_logic::sign::generate_keypair();
        (Zeroizing::new(priv_b58), pub_b58)
    }

    fn advertised(token_name: &str, registry: &str, operator: &str) -> FaucetInstrument {
        FaucetInstrument {
            token_name: token_name.to_string(),
            registry: registry.to_string(),
            operator: operator.to_string(),
            default_amount: String::new(),
        }
    }

    fn held(admin: &str, operator: &str, allowances: &[&str]) -> PreapprovalInfo {
        PreapprovalInfo {
            contract_id: "cid".to_string(),
            operator: operator.to_string(),
            receiver: "me".to_string(),
            instrument_admin: admin.to_string(),
            instrument_allowances: allowances
                .iter()
                .map(|id| orderbook_proto::ledger::InstrumentAllowance { id: id.to_string() })
                .collect(),
        }
    }

    #[test]
    fn test_shared_registry_yields_one_target() {
        let (targets, warnings) = preapproval_targets(
            &[],
            &[
                advertised("USD8", "reg-a", "op"),
                advertised("USX", "reg-a", "op"),
                advertised("CBTC", "reg-b", "op"),
            ],
            "dso",
            "me",
        );
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].admin, "reg-a");
        assert_eq!(targets[0].labels, vec!["USD8", "USX"]);
        assert_eq!(targets[1].admin, "reg-b");
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[test]
    fn test_amulet_label_is_skipped_without_a_known_dso() {
        // dso_party empty, so only the token_name clause can do the skipping.
        let (targets, warnings) = preapproval_targets(
            &[],
            &[
                advertised("Amulet", "dso", "featured"),
                advertised("CBTC", "reg-b", "op"),
            ],
            "",
            "me",
        );
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].admin, "reg-b");
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[test]
    fn test_dso_registry_is_skipped_under_any_label() {
        // Labelled "CC", so only the dso_party clause can do the skipping.
        let (targets, warnings) = preapproval_targets(
            &[],
            &[
                advertised("CC", "dso", "op"),
                advertised("CBTC", "reg-b", "op"),
            ],
            "dso",
            "me",
        );
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].admin, "reg-b");
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[test]
    fn test_another_partys_preapproval_is_not_coverage() {
        let mut theirs = held("reg-a", "op", &[]);
        theirs.receiver = "someone-else".to_string();
        let (targets, _) = preapproval_targets(
            &[theirs],
            &[advertised("USD8", "reg-a", "op")],
            "dso",
            "me",
        );
        assert_eq!(targets.len(), 1, "another party's preapproval must not count");
    }

    #[test]
    fn test_operator_for_registry_rejects_blank_and_unknown() {
        let list = [
            advertised("USD8", "reg-a", "op"),
            advertised("Blank", "reg-blank", ""),
        ];
        assert_eq!(operator_for_registry(&list, "reg-a").unwrap(), "op");

        let err = format!("{:#}", operator_for_registry(&list, "reg-missing").unwrap_err());
        assert!(err.contains("reg-missing"), "got: {err}");
        assert!(err.contains("reg-a"), "lists what is available: {err}");

        assert!(
            operator_for_registry(&list, "reg-blank").is_err(),
            "a blank operator is not usable"
        );
    }

    #[test]
    fn test_cc_renewal_margin_is_thirty_days() {
        let now = chrono::Utc::now();
        assert_eq!(cc_renewal_deadline(now) - now, chrono::Duration::days(30));
    }

    #[test]
    fn test_expires_at_is_read_from_string_and_number_arguments() {
        use orderbook_proto::ledger::ActiveContractInfo;
        let field = |kind| {
            let mut fields = std::collections::BTreeMap::new();
            fields.insert(
                "expiresAt".to_string(),
                prost_types::Value { kind: Some(kind) },
            );
            ActiveContractInfo {
                template_id: "#splice-amulet:Splice.AmuletRules:TransferPreapproval".to_string(),
                create_arguments: Some(prost_types::Struct { fields }),
                ..Default::default()
            }
        };

        let as_string = field(prost_types::value::Kind::StringValue(
            "2030-01-01T00:00:00Z".to_string(),
        ));
        assert_eq!(
            struct_field_string(&as_string, "expiresAt").as_deref(),
            Some("2030-01-01T00:00:00Z")
        );
        assert_eq!(struct_field_string(&as_string, "missing"), None);

        let micros = 1_893_456_000_000_000i64;
        let as_number = field(prost_types::value::Kind::NumberValue(micros as f64));
        let read = struct_field_string(&as_number, "expiresAt").expect("number field");
        assert_eq!(read, micros.to_string());
        assert_eq!(
            parse_daml_timestamp(&read),
            chrono::DateTime::from_timestamp_micros(micros)
        );

        assert_eq!(
            struct_field_string(&ActiveContractInfo::default(), "expiresAt"),
            None
        );
    }

    #[test]
    fn test_incomplete_entries_are_skipped_with_warnings() {
        let (targets, warnings) = preapproval_targets(
            &[],
            &[
                advertised("NoRegistry", "", "op"),
                advertised("NoOperator", "reg-b", ""),
            ],
            "dso",
            "me",
        );
        assert!(targets.is_empty());
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("no registry"));
        assert!(warnings[1].contains("no operator"));
    }

    #[test]
    fn test_unscoped_preapproval_counts_as_coverage() {
        let (targets, warnings) = preapproval_targets(
            &[held("reg-a", "op", &[])],
            &[advertised("USD8", "reg-a", "op")],
            "dso",
            "me",
        );
        assert!(targets.is_empty());
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[test]
    fn test_scoped_preapproval_does_not_count_as_coverage() {
        let (targets, warnings) = preapproval_targets(
            &[held("reg-a", "op", &["USD8"])],
            &[advertised("USD8", "reg-a", "op")],
            "dso",
            "me",
        );
        assert_eq!(targets.len(), 1);
        assert!(warnings.iter().any(|w| w.contains("only for specific")));
    }

    #[test]
    fn test_operator_mismatch_warns_and_leaves_coverage() {
        let (targets, warnings) = preapproval_targets(
            &[held("reg-a", "old-op", &[])],
            &[advertised("USD8", "reg-a", "new-op")],
            "dso",
            "me",
        );
        assert!(targets.is_empty());
        assert!(warnings.iter().any(|w| w.contains("old-op")));
    }

    #[test]
    fn test_conflicting_advertised_operators_warn() {
        let (targets, warnings) = preapproval_targets(
            &[],
            &[
                advertised("USD8", "reg-a", "op1"),
                advertised("USX", "reg-a", "op2"),
            ],
            "dso",
            "me",
        );
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].operator, "op1");
        assert!(warnings.iter().any(|w| w.contains("two operators")));
    }

    #[test]
    fn test_cc_coverage_classification() {
        let now = chrono::Utc::now();
        let deadline = now + chrono::Duration::days(30);
        let accepted = "#splice-amulet:Splice.AmuletRules:TransferPreapproval".to_string();
        let proposal =
            "#splice-wallet:Splice.Wallet.TransferPreapproval:TransferPreapprovalProposal"
                .to_string();
        let far = (now + chrono::Duration::days(365)).to_rfc3339();
        let soon = (now + chrono::Duration::days(2)).to_rfc3339();

        assert_eq!(
            classify_cc_coverage(&[(accepted.clone(), Some(far.clone()))], deadline),
            CcCoverage::Active
        );
        // Expiring inside the margin is renewed, not treated as coverage.
        assert_eq!(
            classify_cc_coverage(&[(accepted.clone(), Some(soon))], deadline),
            CcCoverage::Missing
        );
        assert_eq!(
            classify_cc_coverage(&[(accepted.clone(), None)], deadline),
            CcCoverage::Missing
        );
        assert_eq!(
            classify_cc_coverage(&[(proposal.clone(), None)], deadline),
            CcCoverage::ProposalPending
        );
        // A live accepted contract wins over a stale proposal.
        assert_eq!(
            classify_cc_coverage(
                &[(proposal, None), (accepted.clone(), Some(far))],
                deadline
            ),
            CcCoverage::Active
        );
        assert_eq!(classify_cc_coverage(&[], deadline), CcCoverage::Missing);

        // Microsecond timestamps parse too.
        let micros = ((now + chrono::Duration::days(365)).timestamp_micros()).to_string();
        assert_eq!(
            classify_cc_coverage(&[(accepted, Some(micros))], deadline),
            CcCoverage::Active
        );
    }

    #[test]
    fn test_read_env_value_strips_quotes() {
        let env = scratch_env(Some(
            "PLAIN=v\nDQ=\"v\"\nSQ='v'\nSPACED=  \"v\"  \nINNER=a\"b\n",
        ));
        for key in ["PLAIN", "DQ", "SQ", "SPACED"] {
            assert_eq!(read_env_value(&env, key).as_deref(), Some("v"), "key {key}");
        }
        // Only a matched surrounding pair is stripped.
        assert_eq!(read_env_value(&env, "INNER").as_deref(), Some("a\"b"));
        assert_eq!(read_env_value(&env, "MISSING"), None);
    }

    #[test]
    fn test_recorded_identity_without_key_is_refused() {
        let env = scratch_env(Some("PARTY_AGENT=p::1220ee\n"));
        let err = prepare_onboard_key(&env, None, "http://rpc").unwrap_err();
        assert!(err.to_string().contains("already records an identity"));
        assert!(!std::fs::read_to_string(&env).unwrap().contains("PARTY_AGENT_PRIVATE_KEY"));
    }

    #[test]
    fn test_supplied_key_must_match_recorded_public_key() {
        let (_, pub1) = keypair();
        let (k2, _) = keypair();
        let env = scratch_env(Some(&format!("PARTY_AGENT_PUBLIC_KEY={pub1}\n")));
        let err = prepare_onboard_key(&env, Some(&k2), "http://rpc").unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn test_supplied_key_rejects_stale_different_env_key() {
        let (k1, _) = keypair();
        let (k2, _) = keypair();
        let env = scratch_env(Some(&format!("PARTY_AGENT_PRIVATE_KEY={}\n", &*k1)));
        let err = prepare_onboard_key(&env, Some(&k2), "http://rpc").unwrap_err();
        assert!(err.to_string().contains("different PARTY_AGENT_PRIVATE_KEY"));
    }

    #[test]
    fn test_supplied_key_is_not_written() {
        let (k1, pub1) = keypair();
        let env = scratch_env(None);
        let (pub_b58, key) = prepare_onboard_key(&env, Some(&k1), "http://rpc").unwrap();
        assert_eq!(pub_b58, pub1);
        assert_eq!(&*key, &*k1);
        let content = std::fs::read_to_string(&env).unwrap();
        assert!(!content.contains("PARTY_AGENT_PRIVATE_KEY"));
        assert!(content.contains(&format!("PARTY_AGENT_PUBLIC_KEY={pub1}")));
    }

    #[test]
    fn test_env_key_is_reused() {
        let (k1, pub1) = keypair();
        let env = scratch_env(Some(&format!("PARTY_AGENT_PRIVATE_KEY={}\n", &*k1)));
        let (pub_b58, key) = prepare_onboard_key(&env, None, "http://rpc").unwrap();
        assert_eq!(pub_b58, pub1);
        assert_eq!(&*key, &*k1);
        let content = std::fs::read_to_string(&env).unwrap();
        assert_eq!(content.matches("PARTY_AGENT_PRIVATE_KEY=").count(), 1);
        assert!(content.contains(&format!("PARTY_AGENT_PUBLIC_KEY={pub1}")));
    }

    #[test]
    fn test_quote_key_override_precedence() {
        let kf = atomic_quote::gen_keypair().unwrap();
        let (resolved, source) = resolve_quote_key(Some(&kf.priv_scalar_hex)).unwrap();
        assert_eq!(resolved.pub_spki_hex, kf.pub_spki_hex);
        assert_eq!(source, "--quote-private-key");
        let err = match resolve_quote_key(Some("zz")) {
            Ok(_) => panic!("expected an error"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("--quote-private-key"), "got: {err}");
    }

    #[test]
    fn test_fresh_env_generates_and_writes_key() {
        let env = scratch_env(None);
        let (pub_b58, key) = prepare_onboard_key(&env, None, "http://rpc").unwrap();
        let content = std::fs::read_to_string(&env).unwrap();
        assert!(content.contains(&format!("PARTY_AGENT_PRIVATE_KEY={}", &*key)));
        assert!(content.contains(&format!("PARTY_AGENT_PUBLIC_KEY={pub_b58}")));
        assert!(content.contains("ORDERBOOK_GRPC_URL=http://rpc"));
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;
    use orderbook_proto::ledger::CreatedContractInfo;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn created(template_id: &str, contract_id: &str) -> CreatedContractInfo {
        CreatedContractInfo {
            contract_id: contract_id.to_string(),
            template_id: template_id.to_string(),
            amount: String::new(),
        }
    }

    #[test]
    fn one_amulet_covering_the_total_is_preferred_over_several() {
        let ascending = vec![dec("1"), dec("5"), dec("40"), dec("100")];
        // Smallest single amulet that covers it, not the biggest available.
        let picked = payment_queue::select_amulet_indices(&ascending, dec("30"));
        assert_eq!(picked, vec![2]);
    }

    #[test]
    fn multi_amulet_fallback_takes_the_largest_first() {
        let ascending = vec![dec("1"), dec("2"), dec("30"), dec("40")];
        let picked = payment_queue::select_amulet_indices(&ascending, dec("65"));
        assert_eq!(picked, vec![3, 2]);
    }

    #[test]
    fn an_uncoverable_total_selects_nothing_rather_than_a_partial_set() {
        let ascending = vec![dec("1"), dec("2"), dec("3")];
        assert!(payment_queue::select_amulet_indices(&ascending, dec("100")).is_empty());
    }

    #[test]
    fn a_total_reachable_only_beyond_the_cap_selects_nothing() {
        // 150 x 1 covers 150 overall but only 100 within the cap, so a target
        // between those two numbers must fail rather than over-select.
        let ascending = vec![dec("1"); payment_queue::MAX_AMULET_INPUTS + 50];
        assert!(payment_queue::select_amulet_indices(&ascending, dec("120")).is_empty());

        // The same wallet funds anything the cap can reach.
        let picked = payment_queue::select_amulet_indices(&ascending, dec("80"));
        assert_eq!(picked.len(), 80);
        assert!(picked.len() <= payment_queue::MAX_AMULET_INPUTS);
    }

    #[test]
    fn allocation_selection_prefers_one_amulet_and_returns_largest_first() {
        use crate::holdings_cache::CachedAmulet;
        use std::time::Instant;
        let cached = |cid: &str, amt: &str| CachedAmulet {
            contract_id: cid.to_string(),
            amount: dec(amt),
            discovered_at: Instant::now(),
        };
        // Ascending, as the cache supplies them.
        let selectable = vec![cached("a", "1"), cached("b", "20"), cached("c", "60")];

        // 10 x 1.02 = 10.2 -> the smallest single amulet that covers it.
        let one = payment_queue::select_amulets_for_allocation(&selectable, dec("10"));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].contract_id, "b");

        // 70 x 1.02 = 71.4 -> no single amulet covers it; largest first.
        let many = payment_queue::select_amulets_for_allocation(&selectable, dec("70"));
        assert_eq!(
            many.iter().map(|a| a.contract_id.as_str()).collect::<Vec<_>>(),
            vec!["c", "b"]
        );

        // Nothing can fund it, and a partial set must never be returned.
        assert!(payment_queue::select_amulets_for_allocation(&selectable, dec("500")).is_empty());
        assert!(payment_queue::select_amulets_for_allocation(&selectable, dec("0")).is_empty());
    }

    #[test]
    fn the_fee_margin_raises_the_selection_target() {
        assert_eq!(payment_queue::with_fee_margin(dec("100")), dec("102.00"));
    }

    #[test]
    fn payment_amounts_must_parse_and_be_positive() {
        assert_eq!(parse_payment_amount("10.5", "p", 1).unwrap(), dec("10.5"));

        let bad = parse_payment_amount("1O.5", "p", 3).unwrap_err().to_string();
        assert!(bad.contains("row 3"), "{bad}");
        assert!(bad.contains("1O.5"), "{bad}");

        assert!(parse_payment_amount("0", "p", 1).is_err());
        assert!(parse_payment_amount("-5", "p", 1).is_err());
        assert!(parse_payment_amount("", "p", 1).is_err());
    }

    #[test]
    fn a_created_transfer_instruction_marks_the_transfer_as_pending() {
        let created = vec![
            created("#pkg:Splice.Api.Token.HoldingV1:Holding", "h1"),
            created(
                "#splice-api-token-transfer-instruction-v1:Splice.Api.Token.TransferInstructionV1:TransferInstruction",
                "ti1",
            ),
        ];
        assert_eq!(
            pending_transfer_offer(&created),
            Some(PendingTransfer::RegistryOffer("ti1"))
        );
    }

    #[test]
    fn a_registry_transfer_offer_is_recognised_as_pending() {
        let created = vec![created(
            "#utility-registry-app-v0:Utility.Registry.App.V0.Model.Transfer:TransferOffer",
            "off1",
        )];
        assert_eq!(
            pending_transfer_offer(&created),
            Some(PendingTransfer::RegistryOffer("off1"))
        );
    }

    #[test]
    fn an_amulet_instruction_is_distinguished_from_a_registry_offer() {
        // The CIP-56 accept path cannot consume this one, so it must not be
        // reported as an accept-cip56 candidate.
        let created = vec![created(
            "#splice-amulet:Splice.AmuletTransferInstruction:AmuletTransferInstruction",
            "ati1",
        )];
        assert_eq!(
            pending_transfer_offer(&created),
            Some(PendingTransfer::AmuletInstruction("ati1"))
        );
    }

    #[test]
    fn an_empty_created_list_is_not_treated_as_completion() {
        // The update detail is fetched best-effort, so its absence proves nothing.
        assert_eq!(transfer_outcome(&[]), TransferOutcome::Unknown);
        assert_eq!(
            transfer_outcome(&[created("#pkg:Splice.Amulet:Amulet", "a1")]),
            TransferOutcome::CompletedInOneStep
        );
    }

    #[test]
    fn no_transfer_instruction_means_the_transfer_completed_outright() {
        let created = vec![created("#pkg:Splice.Amulet:Amulet", "a1")];
        assert_eq!(pending_transfer_offer(&created), None);
        assert_eq!(pending_transfer_offer(&[]), None);
    }
}
