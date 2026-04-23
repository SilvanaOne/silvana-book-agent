//! Orderbook Cloud Agent — runs without direct Canton ledger access
//!
//! All ledger operations are proxied through the orderbook gRPC service's
//! DAppProviderService (CIP-0103). The agent signs transaction hashes locally
//! with its Ed25519 private key — the key never leaves the agent.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::Subcommand;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::info;

use agent_logic::config::BaseConfig;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::{
    PrepareTransactionRequest, RequestPreapprovalParams,
    RequestRecurringPrepaidParams, RequestRecurringPayasyougoParams,
    TransferCcParams, TransferCip56Params, AcceptCip56Params, SplitCcParams,
    ExecuteMultiCallParams, MultiCallOp, McBatchPay, McPaymentTarget,
    RequestUserServiceParams, TransactionOperation, TokenBalance,
    LockHoldingsParams, ProcessLockUnlockRequestsParams, ResizeLockParams, TerminateLockParams,
    VotingAllocation as ProtoVotingAllocation, VotingRequest as ProtoVotingRequest,
    FaucetRequest,
    prepare_transaction_request::Params,
    d_app_provider_service_client::DAppProviderServiceClient,
    GetAgentConfigRequest, GetAgentConfigResponse, RegisterAgentRequest,
    GetOnboardingStatusRequest, SubmitOnboardingSignatureRequest, MessageSignature,
    OnboardingStatus as ProtoOnboardingStatus,
};
use tx_verifier::OperationExpectation;

pub mod accept_settle;
pub mod amulet_cache;
pub mod acs_worker;
pub mod backend;
pub mod config;
pub mod fill_loop;
pub mod ledger_client;
pub mod merge_worker;
pub mod payment_queue;
pub mod rfq_handler;

pub use accept_settle::MulticallSettler;
pub use backend::CloudSettlementBackend;
pub use ledger_client::DAppProviderClient;

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
    },
    /// Fetch existing preapprovals
    Fetch,
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
    /// Send CIP-56 instrument token (creates TransferOffer)
    SendCip56 {
        /// Receiver party ID
        #[arg(long)]
        receiver: String,
        /// Instrument name (e.g. "USDC", "CBTC")
        #[arg(long)]
        instrument_id: String,
        /// Registrar party ID (InstrumentId.admin / instrument source)
        #[arg(long)]
        instrument_admin: String,
        /// Amount to send (decimal string)
        #[arg(long)]
        amount: String,
        /// Optional transfer reference
        #[arg(long)]
        reference: Option<String>,
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
        /// Comma-separated input amulet contract IDs (optional — server selects if empty)
        #[arg(long, value_delimiter = ',')]
        amulet_cids: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
pub enum SignCommands {
    /// Sign a 34-byte multihash (Canton format) using Ed25519 key
    Multihash {
        /// Base64-encoded 34-byte multihash
        #[arg(long)]
        input: String,
        /// Private key (base58). Defaults to PARTY_AGENT_PRIVATE_KEY from config
        #[arg(long)]
        private_key: Option<String>,
    },
    /// Sign a text message using Ed25519 key
    Message {
        /// Text message to sign (signed as UTF-8 bytes)
        #[arg(long)]
        input: String,
        /// Private key (base58). Defaults to PARTY_AGENT_PRIVATE_KEY from config
        #[arg(long)]
        private_key: Option<String>,
    },
    /// Sign binary data using Ed25519 key
    Binary {
        /// Hex-encoded binary data (with or without 0x prefix)
        #[arg(long)]
        input: String,
        /// Private key (base58). Defaults to PARTY_AGENT_PRIVATE_KEY from config
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
        /// Token admin party. Optional for Amulet/CC (defaults to $DSO);
        /// required for CIP-56 tokens.
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
        "Traffic fee: {:.2} USD/MB to {:.30}..., join={}",
        config.traffic_fee_usd_per_byte * 1_000_000.0,
        config.traffic_fee_party,
        config.join_traffic
    );
    info!("Fee reserve: {:.2} CC", config.fee_reserve_cc);

    // Create LiquidityManager early so it can be shared with RfqHandler and Backend
    let liquidity_manager = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );

    // Register token aliases from server market data (symbol → instrument_id)
    {
        let mut client = agent_logic::client::OrderbookClient::new(&config).await?;
        match client.get_markets().await {
            Ok(markets) => {
                for market in &markets {
                    let parts: Vec<&str> = market.market_id.split('-').collect();
                    if parts.len() == 2 {
                        liquidity_manager.register_alias(parts[0], &market.base_instrument).await;
                        liquidity_manager.register_alias(parts[1], &market.quote_instrument).await;
                    }
                }
                info!("Registered token aliases from {} markets", markets.len());
            }
            Err(e) => {
                tracing::warn!("Failed to fetch markets for alias registration: {}", e);
            }
        }
    }

    // Create RfqHandler early so we can share its quoted_trades Arc with AgentOptions
    let lp_shutdown = Arc::new(AtomicBool::new(false));
    let quoted_rfq_trades = if config.liquidity_provider.is_some() {
        let mut rfq_handler = rfq_handler::RfqHandler::new(&config)
            .ok_or_else(|| anyhow::anyhow!("Failed to create RFQ handler"))?;
        rfq_handler.set_liquidity_manager(liquidity_manager.clone());
        let quoted_trades = rfq_handler.quoted_trades();

        info!(
            "LP mode enabled: name={}, starting settlement stream",
            config.liquidity_provider.as_ref().unwrap().name
        );
        let config_clone = config.clone();
        let lp_shutdown_clone = lp_shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = run_lp_settlement_stream(config_clone, rfq_handler, lp_shutdown_clone).await {
                tracing::error!("LP settlement stream failed: {}", e);
            }
        });

        Some(quoted_trades)
    } else {
        None
    };

    let confirm_lock = agent_logic::confirm::new_confirm_lock();
    let backend = CloudSettlementBackend::new(config.clone(), verbose, dry_run, force, confirm, confirm_lock, liquidity_manager);

    let ledger_client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
    .context("Failed to create ledger client")?;

    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };

    run_agent(
        config,
        backend,
        balance_provider,
        AgentOptions {
            settlement_only,
            orders_only,
            actionable_count: None,
            shutdown_notify: None,
            accepted_rfq_trades: None,
            rejected_rfq_trades: None,
            quoted_rfq_trades,
            lp_shutdown: Some(lp_shutdown),
            state_file: Some(PathBuf::from("agent-state.json")),
            no_restore,
            fill_state: None,
            no_reject,
        },
    )
    .await
}

/// Run a buyer/seller fill loop with background settlement processing
pub async fn run_fill(
    config: BaseConfig,
    direction: fill_loop::FillDirection,
    market: String,
    amount: f64,
    price_limit: Option<f64>,
    min_settlement: f64,
    max_settlement: Option<f64>,
    interval: u64,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
) -> Result<()> {
    let dir_str = match direction {
        fill_loop::FillDirection::Buy => "buy",
        fill_loop::FillDirection::Sell => "sell",
    };
    info!("Starting {} mode: market={}, amount={}, interval={}s", dir_str, market, amount, interval);

    let confirm_lock = agent_logic::confirm::new_confirm_lock();
    let fill_lm = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    // CloudSettlementBackend spawns the ACS worker which keeps amulet_cache fresh.
    let backend = CloudSettlementBackend::new(
        config.clone(), verbose, dry_run, force, confirm, confirm_lock.clone(), fill_lm,
    );

    // The fill loop doesn't use the settlement-machine / payment-queue paths.
    // It settles each accepted quote atomically via MulticallSettler.
    let settler = Arc::new(MulticallSettler {
        config: config.clone(),
        amulet_cache: backend.amulet_cache().clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock,
    });

    let params = fill_loop::FillParams {
        direction,
        market_id: market,
        total_amount: amount,
        price_limit,
        min_settlement,
        max_settlement: max_settlement.unwrap_or(amount),
        interval_secs: interval,
    };

    // Check for saved fill state
    let state_file = PathBuf::from("agent-state.json");
    let saved_fill_state = agent_logic::state::load_state(&state_file)
        .filter(|s| s.party_id == config.party_id)
        .and_then(|s| s.fill_state);

    // Keep `backend` alive so its ACS worker keeps refreshing amulets until return.
    let _backend_guard = backend;
    fill_loop::run_fill_loop(config, settler, params, saved_fill_state, Some(state_file)).await
}

/// Run the LP settlement stream (bidirectional gRPC for RFQ handling)
pub async fn run_lp_settlement_stream(config: BaseConfig, rfq_handler: rfq_handler::RfqHandler, shutdown: Arc<AtomicBool>) -> Result<()> {
    use orderbook_proto::settlement::{
        settlement_service_client::SettlementServiceClient,
        CantonToServerMessage, SettlementHandshake, CantonNodeAuth, Heartbeat,
        canton_to_server_message::Message as CantonMessage,
        server_to_canton_message::Message as ServerMessage,
    };
    use tokio_stream::StreamExt;
    use rfq_handler::RfqResponse;
    use agent_logic::client::OrderbookClient;

    let lp_config = config.liquidity_provider.as_ref()
        .ok_or_else(|| anyhow::anyhow!("No LP config"))?;

    // Collect RFQ-enabled market_ids for mid-price polling
    let rfq_market_ids: Vec<String> = config
        .markets
        .iter()
        .filter(|m| m.enabled && m.rfq.as_ref().map_or(false, |r| r.enabled))
        .map(|m| m.market_id.clone())
        .collect();

    // Spawn mid-price polling loop so RfqHandler can quote
    let mid_prices = rfq_handler.mid_prices();
    let price_config = config.clone();
    let price_markets = rfq_market_ids.clone();
    let price_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let poll_interval = std::time::Duration::from_secs(10);
        // Initial delay to let the orderbook server start
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let mut client = match OrderbookClient::new(&price_config).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Mid-price poller: failed to create client: {}", e);
                return;
            }
        };

        loop {
            if price_shutdown.load(Ordering::Relaxed) {
                info!("Mid-price poller shutting down");
                return;
            }
            for market_id in &price_markets {
                match client.get_price(market_id).await {
                    Ok(resp) => {
                        let mid = match (resp.bid, resp.ask) {
                            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
                            _ => resp.last,
                        };
                        if mid > 0.0 {
                            mid_prices.write().await.insert(market_id.clone(), mid);
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Mid-price poller: {} error: {}", market_id, e);
                    }
                }
            }
            tokio::time::sleep(poll_interval).await;
        }
    });

    loop {
        if shutdown.load(Ordering::SeqCst) {
            info!("LP stream shutting down, not reconnecting");
            return Ok(());
        }

        info!("Connecting LP settlement stream to {}", config.orderbook_grpc_url);

        let channel = match create_raw_channel(&config.orderbook_grpc_url).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to connect: {}, retrying in 5s", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut client = SettlementServiceClient::new(channel)
            .max_decoding_message_size(16 * 1024 * 1024);

        // Create the outbound channel
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel::<CantonToServerMessage>(64);
        let outbound_stream = tokio_stream::wrappers::ReceiverStream::new(outbound_rx);

        // Open bidirectional stream
        let response = match client.settlement_stream(outbound_stream).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to open settlement stream: {}, retrying in 5s", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        info!("LP settlement stream connected, listening for RFQ requests");

        // Send a heartbeat every 30s. If the outbound send fails, the stream's
        // write half is closed — reconnect. Tonic doesn't surface silent
        // half-closes on the read side, so we can't rely on inbound activity
        // alone; the heartbeat send-failure is what detects a dead stream.
        const HEARTBEAT_INTERVAL_SECS: u64 = 30;
        let mut heartbeat_interval = tokio::time::interval(
            std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS),
        );
        heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick so we don't send a heartbeat 0s after connect.
        heartbeat_interval.tick().await;
        let mut client_seq: u64 = 0;

        loop {
            tokio::select! {
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
                            if shutdown.load(Ordering::SeqCst) {
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

        if shutdown.load(Ordering::SeqCst) {
            info!("LP stream shutting down after disconnect");
            return Ok(());
        }
        tracing::warn!("LP settlement stream disconnected, reconnecting in 5s");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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
        Some(prost_types::value::Kind::NumberValue(n)) => {
            serde_json::Value::Number(serde_json::Number::from_f64(*n).unwrap_or_else(|| serde_json::Number::from(0)))
        }
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
    let mut template_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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
        &config.private_key_bytes,
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
        InfoCommands::Network => {
            let rates = client.get_dso_rates().await?;

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
                    println!("  Featured App:     {}", round.issuance_per_featured_app_reward_coupon);
                    println!("  Unfeatured App:   {}", round.issuance_per_unfeatured_app_reward_coupon);
                    println!("  Validator:        {}", round.issuance_per_validator_reward_coupon);
                    println!("  SV:               {}", round.issuance_per_sv_reward_coupon);
                    if let Some(faucet) = &round.opt_issuance_per_validator_faucet_coupon {
                        println!("  Validator Faucet: {}", faucet);
                    }
                    println!("  Opens At:         {}", round.opens_at);
                    println!("  Target Closes:    {}", round.target_closes_at);
                    println!();
                }
            }
        }
    }

    Ok(())
}

// ============================================================================
// Preapproval commands
// ============================================================================

pub async fn run_preapproval(config: BaseConfig, command: PreapprovalCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        PreapprovalCommands::Request { instrument_admin } => {
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Create CIP-56 Preapproval",
                    &format!("party: {}, admin: {}", config.party_id, instrument_admin),
                ).await?;
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
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            println!("CIP-56 preapproval created, update id: {}", result.update_id);
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
                        p.instrument_allowances.iter()
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
    }

    Ok(())
}

// ============================================================================
// Subscription payment commands
// ============================================================================

pub async fn run_subscription(config: BaseConfig, command: SubscriptionCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
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
                ).await?;
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
                ).await?;
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

pub async fn run_transfer(config: BaseConfig, command: TransferCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
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
                ).await?;
            }
            let command_id = format!("cli-cc-{}", chrono::Utc::now().timestamp_millis());
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
            println!("CC transfer sent: {}", result.update_id);
        }
        TransferCommands::SendCip56 {
            receiver,
            instrument_id,
            instrument_admin,
            amount,
            reference,
        } => {
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    &format!("Transfer CIP-56 ({})", instrument_id),
                    &format!("receiver: {}, amount: {}", receiver, amount),
                ).await?;
            }
            let expectation = OperationExpectation::TransferCip56 {
                sender_party: config.party_id.clone(),
                receiver_party: receiver.clone(),
                instrument_id: instrument_id.clone(),
                instrument_admin: instrument_admin.clone(),
                amount: amount.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::TransferCip56 as i32,
                        params: Some(Params::TransferCip56(TransferCip56Params {
                            instrument_id: instrument_id.clone(),
                            instrument_admin,
                            receiver_party: receiver,
                            amount,
                            reference,
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            println!("CIP-56 transfer sent ({}): {}", instrument_id, result.update_id);
            if let Some(cid) = result.contract_id {
                println!("TransferOffer contract: {}", cid);
            }
        }
        TransferCommands::AcceptCip56 { contract_id } => {
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Accept CIP-56 transfer",
                    &format!("contract: {}", contract_id),
                ).await?;
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
            println!("CIP-56 transfer accepted ({}): {}", contract_id, result.update_id);
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
                    &format!("outputs: [{}], inputs: {} amulets", output_amounts.join(", "), amulet_cids.len()),
                ).await?;
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

            // Print summary
            let total: rust_decimal::Decimal = targets
                .iter()
                .map(|(_, amt)| amt.parse::<rust_decimal::Decimal>().unwrap_or_default())
                .sum();
            println!("Batch pay: {} recipients, total {:.4} CC", targets.len(), total);
            for (i, (receiver, amount)) in targets.iter().enumerate() {
                let short_party = if receiver.len() > 24 {
                    format!("{}...{}", &receiver[..12], &receiver[receiver.len()-8..])
                } else {
                    receiver.clone()
                };
                println!("  {:>3}. {} — {} CC", i + 1, short_party, amount);
            }

            // Auto-select amulets if not provided (same strategy as select_amulets_for_allocation)
            let selected_amulet_cids = if let Some(cids) = amulet_cids {
                cids
            } else {
                let mut amulets = match client.get_amulets().await {
                    Ok(a) => a,
                    Err(_) => client.get_amulets_via_acs().await?,
                };
                amulets.sort_by(|a, b| a.amount.cmp(&b.amount));

                // Prefer ONE amulet that covers the total (smallest-fit)
                let indices: Vec<usize> = if let Some(i) = amulets.iter().position(|a| a.amount >= total) {
                    vec![i]
                } else {
                    // Accumulate smallest-first
                    let mut acc = rust_decimal::Decimal::ZERO;
                    let mut picked = Vec::new();
                    for (i, a) in amulets.iter().enumerate() {
                        picked.push(i);
                        acc += a.amount;
                        if acc >= total { break; }
                    }
                    picked
                };

                let selected_total: rust_decimal::Decimal = indices.iter().map(|&i| amulets[i].amount).sum();
                if selected_total < total {
                    return Err(anyhow::anyhow!(
                        "Insufficient CC: need {:.4} but only {:.4} available across {} amulets",
                        total, selected_total, amulets.len()
                    ));
                }

                println!("Auto-selected {} amulet(s) ({:.4} CC):", indices.len(), selected_total);
                for &i in &indices {
                    let a = &amulets[i];
                    let short = if a.contract_id.len() > 24 {
                        format!("{}...{}", &a.contract_id[..12], &a.contract_id[a.contract_id.len()-8..])
                    } else { a.contract_id.clone() };
                    println!("  {} ({:.4} CC)", short, a.amount);
                }

                indices.into_iter().map(|i| amulets[i].contract_id.clone()).collect()
            };

            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Batch Pay",
                    &format!("{} recipients, total {} CC", targets.len(), total),
                ).await?;
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
            println!("Batch pay completed: {}", result.update_id);
        }
    }

    Ok(())
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
            return Err(anyhow::anyhow!("CSV row {} has fewer than 2 columns", i + 1));
        }
        let party = record[0].trim().to_string();
        let amount = record[1].trim().to_string();

        // Skip header row if present
        if i == 0 && (party.contains("Party") || party.contains("party") || party.contains("Recipient")) {
            continue;
        }

        // Validate amount is a number
        if amount.parse::<rust_decimal::Decimal>().is_err() {
            return Err(anyhow::anyhow!(
                "CSV row {}: invalid amount '{}' for party '{}'",
                i + 1,
                amount,
                party
            ));
        }

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

/// Read a key=value from .env file, returning None if not found or file doesn't exist
pub fn read_env_value(env_file: &std::path::Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(env_file).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(val) = line.strip_prefix(key) {
            if let Some(val) = val.strip_prefix('=') {
                return Some(val.trim().to_string());
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

    std::fs::write(env_file, lines.join("\n") + "\n")
        .context("Failed to write .env file")?;
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
        if line.is_empty() || line.starts_with('#') { continue; }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            // Don't overwrite agent-specific keys
            if matches!(key, "PARTY_AGENT" | "PARTY_AGENT_PRIVATE_KEY" | "PARTY_AGENT_PUBLIC_KEY" | "ORDERBOOK_GRPC_URL") {
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
    let toml_path = env_file.parent().unwrap_or(std::path::Path::new(".")).join("agent.toml");
    if toml_path.exists() {
        println!("agent.toml already exists, skipping write");
        return;
    }
    let mut content = config_resp.agent_toml.clone();
    if let Some(name) = agent_name {
        content = content.replace(
            "name = \"LP agent\"",
            &format!("name = \"LP {}\"", name),
        );
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
        let tls_config = tonic::transport::ClientTlsConfig::new().with_webpki_roots().domain_name(
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
    private_key: Option<String>,
    invite_code: String,
    agent_name: String,
    email: String,
    env_file: PathBuf,
    poll_interval: u64,
) -> Result<()> {
    println!("=== Cloud Agent Self-Service Onboarding ===\n");

    // Step 1: Handle keys
    if let Some(ref pk_b58) = private_key {
        // --private-key provided (with --party): write key and derive public key
        let bytes = agent_logic::config::decode_private_key(pk_b58)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&bytes);
        let pub_b58 = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();
        upsert_env_value(&env_file, "PARTY_AGENT_PRIVATE_KEY", pk_b58)?;
        upsert_env_value(&env_file, "PARTY_AGENT_PUBLIC_KEY", &pub_b58)?;
        upsert_env_value(&env_file, "ORDERBOOK_GRPC_URL", &rpc)?;
        println!("Private key written to {}", env_file.display());
        println!("Public key: {}", pub_b58);
    } else if let Some(pk) = read_env_value(&env_file, "PARTY_AGENT_PRIVATE_KEY") {
        // Existing private key in .env
        println!("Found existing private key in {}", env_file.display());
        let bytes = agent_logic::config::decode_private_key(&pk)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&bytes);
        let pub_b58 = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();
        upsert_env_value(&env_file, "PARTY_AGENT_PUBLIC_KEY", &pub_b58)?;
        println!("Public key: {}", pub_b58);
    } else {
        // No key provided and none in .env — generate new keypair
        println!("Generating new Ed25519 keypair...");
        let (priv_b58, pub_b58) = agent_logic::sign::generate_keypair();
        upsert_env_value(&env_file, "PARTY_AGENT_PRIVATE_KEY", &priv_b58)?;
        upsert_env_value(&env_file, "PARTY_AGENT_PUBLIC_KEY", &pub_b58)?;
        upsert_env_value(&env_file, "ORDERBOOK_GRPC_URL", &rpc)?;
        println!("Private key written to {}", env_file.display());
        println!("Public key: {}", pub_b58);
    };

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
            return complete_ledger_onboarding(&env_file, &rpc).await;
        }
    }

    // Need private key bytes and public key for the waiting list flow
    let pk = read_env_value(&env_file, "PARTY_AGENT_PRIVATE_KEY")
        .ok_or_else(|| anyhow::anyhow!("PARTY_AGENT_PRIVATE_KEY missing from .env"))?;
    let private_key_bytes = agent_logic::config::decode_private_key(&pk)?;
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&private_key_bytes);
    let public_key_b58 = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();

    // Step 2: Connect to RPC (raw channel, no JWT auth needed)
    println!("\nConnecting to {}...", rpc);
    let channel = create_raw_channel(&rpc).await?;
    let mut client = DAppProviderServiceClient::new(channel)
        .max_decoding_message_size(16 * 1024 * 1024);
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
    let canonical = message_signing::canonical_register_agent(
        &public_key_b58,
        Some(&invite_code),
    );
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

    println!("Registration: {} (id={})", register_resp.message, register_resp.waiting_list_id);

    // Step 5: Poll for SIGNATURE_REQUIRED status
    let mut current_status = register_resp.status;
    loop {
        if current_status == ProtoOnboardingStatus::SignatureRequired as i32
            || current_status == ProtoOnboardingStatus::TopologyCreated as i32
            || current_status == ProtoOnboardingStatus::Onboarded as i32
        {
            break;
        }

        println!("Status: REQUESTED — waiting for backend to create multihash (polling every {}s)...", poll_interval);
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

        let multihash = status_resp.multihash
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Status is SIGNATURE_REQUIRED but no multihash was provided"))?;

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

        println!("Status: SIGNATURE_SUBMITTED — waiting for topology creation (polling every {}s)...", poll_interval);
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
            let party_id = status_resp.party_id
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Status is TOPOLOGY_CREATED but no party_id was provided"))?;
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

        let party_id = status_resp.party_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Onboarding completed but no party_id was provided"))?;
        upsert_env_value(&env_file, "PARTY_AGENT", &party_id)?;
        println!("\nPARTY_AGENT={}", party_id);
    }

    // Steps 9-11: Complete ledger onboarding
    complete_ledger_onboarding(&env_file, &rpc).await
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
    pub private_key_bytes: [u8; 32],
    pub node_name: String,
    pub ledger_service_public_key: [u8; 32],
    pub token_ttl_secs: u64,
    pub connection_timeout_secs: u64,
    pub request_timeout_secs: u64,
}

impl OnboardConfig {
    pub fn from_env() -> Result<Self> {
        let party_id = std::env::var("PARTY_AGENT")
            .map_err(|_| anyhow::anyhow!("PARTY_AGENT env var is required"))?;
        let private_key_base58 = std::env::var("PARTY_AGENT_PRIVATE_KEY")
            .map_err(|_| anyhow::anyhow!("PARTY_AGENT_PRIVATE_KEY env var is required"))?;
        let private_key_bytes = agent_logic::config::decode_private_key(&private_key_base58)?;
        let node_name = std::env::var("NODE_NAME")
            .map_err(|_| anyhow::anyhow!("NODE_NAME env var is required"))?;
        let ledger_service_public_key_base58 = std::env::var("LEDGER_SERVICE_PUBLIC_KEY")
            .map_err(|_| anyhow::anyhow!("LEDGER_SERVICE_PUBLIC_KEY env var is required"))?;
        let ledger_service_public_key = agent_logic::config::decode_public_key(
            &ledger_service_public_key_base58,
        )?;

        Ok(Self {
            party_id,
            role: std::env::var("AGENT_ROLE").unwrap_or_else(|_| "agent".to_string()),
            private_key_bytes,
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
) -> Result<()> {
    // Ensure .env is loaded into process env
    let _ = dotenvy::from_path(env_file);

    let cfg = OnboardConfig::from_env()
        .context("Failed to load onboarding config from .env")?;

    println!("\nCompleting ledger onboarding for party {}...", cfg.party_id);

    let mut client = DAppProviderClient::new(
        rpc,
        &cfg.party_id,
        &cfg.role,
        &cfg.private_key_bytes,
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

    // 11a-CC: Create Splice TransferPreapproval for CC (required for CC auto-completion).
    // Check if Splice preapproval or proposal already exists.
    let splice_preapproval_templates = [
        "#splice-amulet:Splice.AmuletRules:TransferPreapproval".to_string(),
        "#splice-wallet:Splice.Wallet.TransferPreapproval:TransferPreapprovalProposal".to_string(),
    ];
    let splice_contracts = client.get_active_contracts(&splice_preapproval_templates).await.unwrap_or_default();
    if splice_contracts.is_empty() {
        println!("Creating Splice preapproval for CC...");
        let dso_for_cc = read_env_value(env_file, "DSO").unwrap_or_default();
        if !dso_for_cc.is_empty() {
            let expectation = OperationExpectation::RequestPreapproval {
                party: cfg.party_id.clone(),
            };
            match client.submit_transaction(
                PrepareTransactionRequest {
                    operation: TransactionOperation::RequestPreapproval as i32,
                    params: Some(Params::RequestPreapproval(RequestPreapprovalParams {
                        instrument_admin: dso_for_cc,
                        instrument_allowances: vec![],
                    })),
                    request_signature: None,
                },
                &expectation,
                false, false, false,
            ).await {
                Ok(_) => println!("Splice preapproval proposal created for CC (pending operator acceptance)."),
                Err(e) => println!("Warning: failed to create CC preapproval: {}", e),
            }
        }
    } else {
        println!("Splice CC preapproval already exists ({} found).", splice_contracts.len());
    }

    // 11a-CIP56: Create CIP-56 TransferPreapprovals for utility tokens.
    let preapprovals = client.get_preapprovals().await?;
    let existing_admins: std::collections::HashSet<&str> = preapprovals.iter()
        .map(|p| p.instrument_admin.as_str())
        .collect();

    // Fetch instruments from orderbook-rpc to discover all registrars.
    // Each unique registry party needs one CIP-56 TransferPreapproval.
    let instruments = {
        use orderbook_proto::orderbook::orderbook_service_client::OrderbookServiceClient;
        use orderbook_proto::orderbook::GetInstrumentsRequest;
        let channel = create_raw_channel(rpc).await
            .context("Failed to connect to orderbook-rpc for instruments")?;
        let jwt = agent_logic::auth::generate_jwt(
            &cfg.party_id, &cfg.role, &cfg.private_key_bytes,
            cfg.token_ttl_secs, Some(cfg.node_name.as_str()),
        ).unwrap_or_default();
        let mut ob = OrderbookServiceClient::new(channel);
        let mut req = tonic::Request::new(GetInstrumentsRequest {
            instrument_type: None, limit: None, offset: None,
        });
        if !jwt.is_empty() {
            req.metadata_mut().insert("authorization",
                format!("Bearer {}", jwt).parse().unwrap());
        }
        ob.get_instruments(req).await
            .map(|r| r.into_inner().instruments)
            .unwrap_or_default()
    };

    // Deduplicate registrars — skip DSO (CC uses Splice preapproval, not CIP-56)
    let dso_party = read_env_value(env_file, "DSO").unwrap_or_default();
    let mut needed_admins: Vec<(String, String)> = Vec::new();
    let mut seen_admins: std::collections::HashSet<String> = existing_admins.iter()
        .map(|s| s.to_string())
        .collect();
    for inst in &instruments {
        if let Some(ref registry) = inst.registry {
            if !registry.is_empty() && registry != &dso_party && seen_admins.insert(registry.clone()) {
                needed_admins.push((registry.clone(), inst.instrument_id.clone()));
            }
        }
    }

    if needed_admins.is_empty() {
        if preapprovals.is_empty() {
            println!("Warning: could not determine needed preapprovals (no instruments found). Run onboard again if needed.");
        } else {
            println!("CIP-56 preapprovals up to date ({} found).", preapprovals.len());
        }
    } else {
        for (admin, label) in &needed_admins {
            println!("Creating CIP-56 preapproval for {} (admin={})...", label, admin);
            let label = label.as_str();
            let expectation = OperationExpectation::RequestPreapproval {
                party: cfg.party_id.clone(),
            };
            client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestPreapproval as i32,
                        params: Some(Params::RequestPreapproval(RequestPreapprovalParams {
                            instrument_admin: admin.clone(),
                            instrument_allowances: vec![],
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    false,
                    false,
                    false,
                )
                .await
                .context(format!("Failed to create CIP-56 preapproval for {}", label))?;
            println!("CIP-56 preapproval created for {}.", label);
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
        println!("User service already exists or pending ({} contracts found).", user_contracts.len());
    }

    // Devnet: auto-faucet CC and USDC to bootstrap agent balance
    let canton_chain = read_env_value(env_file, "CANTON_CHAIN").unwrap_or_default();
    if canton_chain == "devnet" {
        let dso = read_env_value(env_file, "DSO").unwrap_or_default();
        let balances = client.get_balances().await.unwrap_or_default();
        let has_cc = balances.iter().any(|b| b.is_canton_coin);
        let has_usdc = balances.iter().any(|b| b.instrument_id == "USDC");
        if !dso.is_empty() && (!has_cc || !has_usdc) {
            println!("\nDevnet detected — requesting faucet top-up...");

            // USDC first (CIP-56 preapproval is already active, no waiting needed)
            if has_usdc {
                println!("  USDC: already funded, skipping");
            } else {
                let usdc_admin = {
                    use orderbook_proto::orderbook::orderbook_service_client::OrderbookServiceClient;
                    use orderbook_proto::orderbook::GetInstrumentsRequest;
                    let mut found = None;
                    if let Ok(ch) = create_raw_channel(rpc).await {
                        let jwt = agent_logic::auth::generate_jwt(
                            &cfg.party_id, &cfg.role, &cfg.private_key_bytes,
                            cfg.token_ttl_secs, Some(cfg.node_name.as_str()),
                        ).unwrap_or_default();
                        let mut ob = OrderbookServiceClient::new(ch);
                        let mut req = tonic::Request::new(GetInstrumentsRequest {
                            instrument_type: None, limit: None, offset: None,
                        });
                        if !jwt.is_empty() {
                            req.metadata_mut().insert("authorization",
                                format!("Bearer {}", jwt).parse().unwrap());
                        }
                        if let Ok(resp) = ob.get_instruments(req).await {
                            for inst in resp.into_inner().instruments {
                                if inst.instrument_id == "USDC" {
                                    found = inst.registry;
                                    break;
                                }
                            }
                        }
                    }
                    found
                };
                if let Some(admin) = usdc_admin {
                    print!("  USDC... ");
                    match client.request_faucet(FaucetRequest {
                        token_name: "USDC".into(),
                        token_admin: admin,
                        ticket: String::new(),
                        amount: String::new(),
                        dry_run: false,
                        request_signature: None,
                    }).await {
                        Ok(r) if r.success => println!("OK ({})", r.amount_approved),
                        Ok(r) => println!("failed: {}", r.error_message.unwrap_or_default()),
                        Err(e) => println!("error: {}", e),
                    }
                }
            }

            // CC last with retry (Splice preapproval may still be pending operator acceptance)
            if has_cc {
                println!("  CC: already funded, skipping");
            } else {
                let mut cc_ok = false;
                let delays = [0u64, 10, 15, 20, 25, 30, 30, 30, 30];
                for attempt in 0..delays.len() as u64 {
                    if attempt > 0 {
                        let delay = delays[attempt as usize];
                        println!("  CC: retrying in {}s (waiting for preapproval acceptance)...", delay);
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    }
                    print!("  CC (Amulet)... ");
                    match client.request_faucet(FaucetRequest {
                        token_name: "Amulet".into(),
                        token_admin: dso.clone(),
                        ticket: String::new(),
                        amount: String::new(),
                        dry_run: false,
                        request_signature: None,
                    }).await {
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
                    println!("  CC faucet failed after retries. Run './cloud-agent faucet get --token CC' manually.");
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
                        let name = if b.is_canton_coin { "CC".to_string() } else { b.instrument_id.clone() };
                        println!("  {}: {}", name, b.total_amount);
                    }
                    break;
                }
            }
        } else if has_cc && has_usdc {
            println!("\nBalances already funded, skipping faucet.");
        }
    }

    println!("\n=== Onboarding complete! ===");
    Ok(())
}

// ============================================================================
// User service commands
// ============================================================================

pub async fn run_user_service(config: BaseConfig, command: UserServiceCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;

    match command {
        UserServiceCommands::Request { reference_id, party_name } => {
            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Request UserService",
                    &format!("party: {}", config.party_id),
                ).await?;
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

pub fn run_sign(config: BaseConfig, command: SignCommands) -> Result<()> {
    match command {
        SignCommands::Multihash { input, private_key } => {
            let key = agent_logic::sign::resolve_signing_key(
                &config.private_key_bytes,
                private_key.as_deref(),
            )?;
            println!("{}", agent_logic::sign::sign_multihash(&key, &input)?);
        }
        SignCommands::Message { input, private_key } => {
            let key = agent_logic::sign::resolve_signing_key(
                &config.private_key_bytes,
                private_key.as_deref(),
            )?;
            println!("{}", agent_logic::sign::sign_message(&key, &input));
        }
        SignCommands::Binary { input, private_key } => {
            let key = agent_logic::sign::resolve_signing_key(
                &config.private_key_bytes,
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
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    ).await?;

    match command {
        FaucetCommands::Get { token, admin, ticket, amount, dry_run } => {
            // "CC" and "Amulet" (case-insensitive) both mean Canton Coin — normalize
            // to "Amulet" before signing, since the server canonical includes token_name.
            let is_cc = token.eq_ignore_ascii_case("cc") || token.eq_ignore_ascii_case("amulet");
            let normalized_token = if is_cc { "Amulet".to_string() } else { token };

            let resolved_admin = match admin {
                Some(a) => a,
                None if is_cc => config.dso_party.clone(),
                None => {
                    anyhow::bail!(
                        "--admin is required for CIP-56 tokens. For Canton Coin use \
                         --token CC (or Amulet) and --admin will default to the DSO \
                         party from $DSO."
                    );
                }
            };

            let response = client.request_faucet(FaucetRequest {
                token_name: normalized_token,
                token_admin: resolved_admin,
                ticket,
                amount: amount.unwrap_or_default(),
                dry_run,
                request_signature: None,
            }).await?;

            if response.success {
                if response.is_dry_run {
                    println!("Faucet dry run approved:");
                } else {
                    println!("Faucet transfer completed:");
                    println!("  Update ID: {}", response.update_id);
                }
                println!("  Token:     {} (admin: {})", response.token_name, response.token_admin);
                println!("  Amount:    {}", response.amount_approved);
            } else {
                println!("Faucet request failed: {}", response.error_message.as_deref().unwrap_or("unknown"));
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

pub async fn run_lock(config: BaseConfig, command: LockCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = ledger_client::DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    ).await?;

    match command {
        LockCommands::Holdings {
            lock_service_cid, lock_service_template_id, lock_service_blob,
            amount, instrument_id, context, allocations_file,
            fee_parties, fee_amounts, amulet_cids,
        } => {
            let context = context.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

            // Load allocations from file if provided
            let allocations: Vec<ProtoVotingAllocation> = match allocations_file {
                Some(path) => {
                    let content = std::fs::read_to_string(&path)
                        .with_context(|| format!("Failed to read allocations file '{}'", path))?;
                    let allocs: Vec<serde_json::Value> = serde_json::from_str(&content)
                        .with_context(|| format!("Invalid allocations JSON in '{}'", path))?;
                    allocs.iter().map(|a| ProtoVotingAllocation {
                        context: a.get("context").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        amount: a.get("amount").and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                    }).collect()
                }
                None => vec![],
            };

            if confirm && !dry_run {
                let lock = agent_logic::confirm::new_confirm_lock();
                agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Lock Holdings",
                    &format!("amount: {}, instrument: {}, context: {}", amount, instrument_id, context),
                ).await?;
            }

            let expectation = OperationExpectation::LockHoldings {
                party: config.party_id.clone(),
                amount: amount.clone(),
                instrument_id: instrument_id.clone(),
                context: context.clone(),
            };

            let result = client.submit_transaction(
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
            ).await?;

            println!("Lock holdings submitted, update id: {}", result.update_id);
            println!("  Context: {}", context);
        }
        LockCommands::Vote {
            lock_controller_cid, lock_controller_template_id, requests_file,
            fee_parties, fee_amounts, amulet_cids,
        } => {
            let content = std::fs::read_to_string(&requests_file)
                .with_context(|| format!("Failed to read requests file '{}'", requests_file))?;
            let reqs_json: Vec<serde_json::Value> = serde_json::from_str(&content)
                .with_context(|| format!("Invalid requests JSON in '{}'", requests_file))?;
            let requests: Vec<ProtoVotingRequest> = reqs_json.iter().map(|r| ProtoVotingRequest {
                context: r.get("context").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                amount: r.get("amount").and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                direction: r.get("direction").and_then(|v| v.as_str()).unwrap_or("VoteLock").to_string(),
            }).collect();

            let expectation = OperationExpectation::ProcessLockUnlockRequests {
                party: config.party_id.clone(),
                request_count: requests.len(),
            };

            let result = client.submit_transaction(
                PrepareTransactionRequest {
                    operation: TransactionOperation::ProcessLockUnlockRequests as i32,
                    params: Some(Params::ProcessLockUnlockRequests(ProcessLockUnlockRequestsParams {
                        lock_controller_cid,
                        lock_controller_template_id,
                        requests,
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
            ).await?;

            println!("Vote submitted, update id: {}", result.update_id);
        }
        LockCommands::Resize {
            lock_controller_cid, lock_controller_template_id, new_amount, instrument_id,
            requests_file, fee_parties, fee_amounts, amulet_cids,
        } => {
            let requests: Vec<ProtoVotingRequest> = match requests_file {
                Some(path) => {
                    let content = std::fs::read_to_string(&path)
                        .with_context(|| format!("Failed to read requests file '{}'", path))?;
                    let reqs_json: Vec<serde_json::Value> = serde_json::from_str(&content)
                        .with_context(|| format!("Invalid requests JSON in '{}'", path))?;
                    reqs_json.iter().map(|r| ProtoVotingRequest {
                        context: r.get("context").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        amount: r.get("amount").and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                        direction: r.get("direction").and_then(|v| v.as_str()).unwrap_or("VoteLock").to_string(),
                    }).collect()
                }
                None => vec![],
            };

            let expectation = OperationExpectation::ResizeLock {
                party: config.party_id.clone(),
                new_amount: new_amount.clone(),
            };

            let result = client.submit_transaction(
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
            ).await?;

            println!("Resize lock submitted, update id: {}", result.update_id);
        }
        LockCommands::Terminate {
            lock_controller_cid, lock_controller_template_id,
            fee_parties, fee_amounts, amulet_cids,
        } => {
            let expectation = OperationExpectation::TerminateLock {
                party: config.party_id.clone(),
            };

            let result = client.submit_transaction(
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
            ).await?;

            println!("Terminate lock submitted, update id: {}", result.update_id);
        }
    }

    Ok(())
}
