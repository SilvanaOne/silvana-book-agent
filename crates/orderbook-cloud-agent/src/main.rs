//! Orderbook Cloud Agent — runs without direct Canton ledger access
//!
//! All ledger operations are proxied through the orderbook gRPC service's
//! DAppProviderService (CIP-0103). The agent signs transaction hashes locally
//! with its Ed25519 private key — the key never leaves the agent.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::info;

use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::{
    PrepareTransactionRequest, RequestPreapprovalParams,
    RequestRecurringPrepaidParams, RequestRecurringPayasyougoParams,
    TransferCcParams, TransferCip56Params, AcceptCip56Params, SplitCcParams,
    RequestUserServiceParams, TransactionOperation, TokenBalance,
    prepare_transaction_request::Params,
    d_app_provider_service_client::DAppProviderServiceClient,
    GetAgentConfigRequest, GetAgentConfigResponse, RegisterAgentRequest,
    GetOnboardingStatusRequest, SubmitOnboardingSignatureRequest, MessageSignature,
    OnboardingStatus as ProtoOnboardingStatus,
};
use tx_verifier::OperationExpectation;

mod amulet_cache;
mod acs_worker;
mod backend;
mod config;
mod fill_loop;
mod ledger_client;
mod payment_queue;
mod rfq_handler;

use backend::CloudSettlementBackend;
use ledger_client::DAppProviderClient;

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "cloud-agent")]
#[command(about = "Orderbook cloud agent — runs without direct ledger access")]
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
    /// Buy a specified amount via RFQ, repeating until filled
    Buyer {
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
    Seller {
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
    Onboard {
        /// Orderbook gRPC URL to connect to
        #[arg(long)]
        rpc: String,
        /// Party ID (skip waiting list, go straight to ledger onboarding). Requires --private-key.
        #[arg(long, requires = "private_key")]
        party: Option<String>,
        /// Base58-encoded Ed25519 private key (required when --party is provided)
        #[arg(long)]
        private_key: Option<String>,
        /// Invite code for waiting list registration
        #[arg(long)]
        invite_code: Option<String>,
        /// Agent display name
        #[arg(long)]
        agent_name: Option<String>,
        /// Contact email
        #[arg(long)]
        email: Option<String>,
        /// Path to .env file to create/update (default: .env)
        #[arg(long, default_value = ".env")]
        env_file: PathBuf,
        /// Seconds between status polls (default: 10)
        #[arg(long, default_value = "10")]
        poll_interval: u64,
    },
}

#[derive(Subcommand)]
enum InfoCommands {
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
enum PreapprovalCommands {
    /// Request a TransferPreapproval
    Request,
    /// Fetch existing preapprovals
    Fetch,
}

#[derive(Subcommand)]
enum TransferCommands {
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
}

#[derive(Subcommand)]
enum SignCommands {
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
enum UserServiceCommands {
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
enum SubscriptionCommands {
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

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    // Initialize logging (LOG_DESTINATION=console|file)
    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["cloud_agent", "orderbook_agent_logic", "tx_verifier"],
        "cloud-agent",
    );

    // Handle commands that don't need config first
    if let Commands::GeneratePrivateKey = &cli.command {
        return run_generate_private_key();
    }

    if let Commands::Onboard { rpc, party, private_key, invite_code, agent_name, email, env_file, poll_interval } = cli.command {
        return run_onboard(rpc, party, private_key, invite_code, agent_name, email, env_file, poll_interval, cli.config).await;
    }

    let base_config = config::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    let verbose = cli.verbose;
    let dry_run = cli.dry_run;
    let force = cli.force;
    let confirm = cli.confirm;
    match cli.command {
        Commands::Agent {
            settlement_only,
            orders_only,
            no_restore,
            no_reject,
        } => run_cloud_agent(base_config, settlement_only, orders_only, no_restore, no_reject, verbose, dry_run, force, confirm).await,
        Commands::Info { command } => run_info(base_config, command).await,
        Commands::Preapproval { command } => run_preapproval(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Subscription { command } => run_subscription(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Transfer { command } => run_transfer(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Sign { command } => run_sign(base_config, command),
        Commands::UserService { command } => run_user_service(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Buyer { market, amount, price_limit, min_settlement, max_settlement, interval } => {
            run_fill(base_config, fill_loop::FillDirection::Buy, market, amount, price_limit, min_settlement, max_settlement, interval, verbose, dry_run, force, confirm).await
        }
        Commands::Seller { market, amount, price_limit, min_settlement, max_settlement, interval } => {
            run_fill(base_config, fill_loop::FillDirection::Sell, market, amount, price_limit, min_settlement, max_settlement, interval, verbose, dry_run, force, confirm).await
        }
        Commands::GeneratePrivateKey => unreachable!(),
        Commands::Onboard { .. } => unreachable!(),
    }
}

// ============================================================================
// Agent mode
// ============================================================================

/// Balance provider that fetches holdings via DAppProviderService gRPC proxy
struct CloudBalanceProvider {
    client: TokioMutex<DAppProviderClient>,
}

#[async_trait]
impl BalanceProvider for CloudBalanceProvider {
    async fn fetch_balances(&self) -> Result<Vec<TokenBalance>> {
        self.client.lock().await.get_balances().await
    }
}

async fn run_cloud_agent(
    config: BaseConfig,
    settlement_only: bool,
    orders_only: bool,
    no_restore: bool,
    no_reject: bool,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
) -> Result<()> {
    info!("Starting Orderbook Cloud Agent");
    info!("Party ID: {}", config.party_id);
    info!("Orderbook URL: {}", config.orderbook_grpc_url);
    info!(
        "Traffic fee: {:.2} USD/MB to {:.30}..., join={}",
        config.traffic_fee_usd_per_byte * 1_000_000.0,
        config.traffic_fee_party,
        config.join_traffic
    );
    info!("Fee reserve: {:.2} CC", config.fee_reserve_cc);

    // Create RfqHandler early so we can share its quoted_trades Arc with AgentOptions
    let lp_shutdown = Arc::new(AtomicBool::new(false));
    let quoted_rfq_trades = if config.liquidity_provider.is_some() {
        let rfq_handler = rfq_handler::RfqHandler::new(&config)
            .ok_or_else(|| anyhow::anyhow!("Failed to create RFQ handler"))?;
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

    let confirm_lock = orderbook_agent_logic::confirm::new_confirm_lock();
    let backend = CloudSettlementBackend::new(config.clone(), verbose, dry_run, force, confirm, confirm_lock);

    let ledger_client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
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
async fn run_fill(
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
        fill_loop::FillDirection::Buy => "buyer",
        fill_loop::FillDirection::Sell => "seller",
    };
    info!("Starting {} mode: market={}, amount={}, interval={}s", dir_str, market, amount, interval);

    let confirm_lock = orderbook_agent_logic::confirm::new_confirm_lock();
    let backend = CloudSettlementBackend::new(config.clone(), verbose, dry_run, force, confirm, confirm_lock);

    let ledger_client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
    )
    .await
    .context("Failed to create ledger client")?;

    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };

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
    let saved_fill_state = orderbook_agent_logic::state::load_state(&state_file)
        .filter(|s| s.party_id == config.party_id)
        .and_then(|s| s.fill_state);

    fill_loop::run_fill_loop(config, backend, balance_provider, params, saved_fill_state, Some(state_file)).await
}

/// Run the LP settlement stream (bidirectional gRPC for RFQ handling)
async fn run_lp_settlement_stream(config: BaseConfig, rfq_handler: rfq_handler::RfqHandler, shutdown: Arc<AtomicBool>) -> Result<()> {
    use orderbook_proto::settlement::{
        settlement_service_client::SettlementServiceClient,
        CantonToServerMessage, SettlementHandshake, CantonNodeAuth,
        canton_to_server_message::Message as CantonMessage,
        server_to_canton_message::Message as ServerMessage,
    };
    use tokio_stream::StreamExt;
    use rfq_handler::RfqResponse;
    use orderbook_agent_logic::client::OrderbookClient;

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

        let mut client = SettlementServiceClient::new(channel);

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

        // Process incoming messages
        while let Some(msg_result) = inbound.next().await {
            let msg = match msg_result {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("Stream error: {}", e);
                    break;
                }
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
                    // Ignore heartbeats
                }
                other => {
                    tracing::debug!("LP stream received unhandled message: {:?}", other.map(|_| "..."));
                }
            }
        }

        tracing::warn!("LP settlement stream disconnected, reconnecting in 5s");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

// ============================================================================
// Info commands
// ============================================================================

fn prost_struct_to_json(s: &prost_types::Struct) -> serde_json::Value {
    let map = s
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), prost_value_to_json(v)))
        .collect();
    serde_json::Value::Object(map)
}

fn prost_value_to_json(v: &prost_types::Value) -> serde_json::Value {
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

fn format_value(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        _ => val.to_string(),
    }
}

fn print_json_value(value: &serde_json::Value, indent: usize) {
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

fn print_info_list_table(contracts: &[orderbook_proto::ledger::ActiveContractInfo]) {
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

fn print_info_list_count(contracts: &[orderbook_proto::ledger::ActiveContractInfo]) {
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

async fn run_info(config: BaseConfig, command: InfoCommands) -> Result<()> {
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

async fn run_preapproval(config: BaseConfig, command: PreapprovalCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
    )
    .await?;

    match command {
        PreapprovalCommands::Request => {
            if confirm && !dry_run {
                let lock = orderbook_agent_logic::confirm::new_confirm_lock();
                orderbook_agent_logic::confirm::confirm_transaction(
                    &lock,
                    "Request Preapproval",
                    &format!("party: {}", config.party_id),
                ).await?;
            }
            let expectation = OperationExpectation::RequestPreapproval {
                party: config.party_id.clone(),
            };
            let result = client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestPreapproval as i32,
                        params: Some(Params::RequestPreapproval(RequestPreapprovalParams {})),
                        request_signature: None,
                    },
                    &expectation,
                    verbose,
                    dry_run,
                    force,
                )
                .await?;
            println!("Preapproval requested, update id: {}", result.update_id);
        }
        PreapprovalCommands::Fetch => {
            let preapprovals = client.get_preapprovals().await?;
            if preapprovals.is_empty() {
                println!("No preapprovals found");
            } else {
                for p in &preapprovals {
                    println!(
                        "{}: {} -> {} (source: {})",
                        p.contract_id, p.provider, p.receiver, p.source
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

async fn run_subscription(config: BaseConfig, command: SubscriptionCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
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
                let lock = orderbook_agent_logic::confirm::new_confirm_lock();
                orderbook_agent_logic::confirm::confirm_transaction(
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
                let lock = orderbook_agent_logic::confirm::new_confirm_lock();
                orderbook_agent_logic::confirm::confirm_transaction(
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

async fn run_transfer(config: BaseConfig, command: TransferCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
    )
    .await?;

    match command {
        TransferCommands::SendCc {
            receiver,
            amount,
            description,
        } => {
            if confirm && !dry_run {
                let lock = orderbook_agent_logic::confirm::new_confirm_lock();
                orderbook_agent_logic::confirm::confirm_transaction(
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
                let lock = orderbook_agent_logic::confirm::new_confirm_lock();
                orderbook_agent_logic::confirm::confirm_transaction(
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
                let lock = orderbook_agent_logic::confirm::new_confirm_lock();
                orderbook_agent_logic::confirm::confirm_transaction(
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
                let lock = orderbook_agent_logic::confirm::new_confirm_lock();
                orderbook_agent_logic::confirm::confirm_transaction(
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
    }

    Ok(())
}

// ============================================================================
// Sign commands
// ============================================================================

fn run_generate_private_key() -> Result<()> {
    let (private_key, public_key) = orderbook_agent_logic::sign::generate_keypair();
    println!("PARTY_AGENT_PRIVATE_KEY={}", private_key);
    println!("PARTY_AGENT_PUBLIC_KEY={}", public_key);
    Ok(())
}

// ============================================================================
// Onboard command — self-service agent onboarding
// ============================================================================

/// Read a key=value from .env file, returning None if not found or file doesn't exist
fn read_env_value(env_file: &std::path::Path, key: &str) -> Option<String> {
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
fn upsert_env_value(env_file: &std::path::Path, key: &str, value: &str) -> Result<()> {
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
fn write_server_config_to_env(
    env_file: &std::path::Path,
    rpc: &str,
    config_resp: &GetAgentConfigResponse,
) -> Result<()> {
    upsert_env_value(env_file, "ORDERBOOK_GRPC_URL", rpc)?;
    upsert_env_value(env_file, "SYNCHRONIZER_ID", &config_resp.synchronizer_id)?;
    upsert_env_value(env_file, "PARTY_SETTLEMENT_OPERATOR", &config_resp.settlement_operator)?;
    upsert_env_value(env_file, "PARTY_TRAFFIC_FEE", &config_resp.traffic_fee_party)?;
    upsert_env_value(env_file, "TRAFFIC_FEE_PRICE_USD_MB", &config_resp.traffic_fee_price_usd_mb)?;
    upsert_env_value(env_file, "PARTY_ORDERBOOK_FEE", &config_resp.fee_party)?;
    upsert_env_value(env_file, "NODE_NAME", &config_resp.node_name)?;
    upsert_env_value(env_file, "LEDGER_SERVICE_PUBLIC_KEY", &config_resp.ledger_service_public_key)?;
    upsert_env_value(env_file, "JOIN_TRAFFIC_TRANSACTIONS", &config_resp.join_traffic_transactions)?;
    upsert_env_value(env_file, "AGENT_FEE_RESERVE_CC", &config_resp.agent_fee_reserve_cc)?;
    if !config_resp.recurring_payment_package_name.is_empty() {
        upsert_env_value(env_file, "RECURRING_PAYMENT_PACKAGE_NAME", &config_resp.recurring_payment_package_name)?;
    }
    Ok(())
}

/// Create a raw (unauthenticated) gRPC channel
async fn create_raw_channel(grpc_url: &str) -> Result<tonic::transport::Channel> {
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
fn sign_onboarding_request(private_key_bytes: &[u8; 32], canonical: &[u8]) -> MessageSignature {
    let sig_data = message_signing::sign_canonical(private_key_bytes, canonical);
    MessageSignature {
        signature: sig_data.signature_b64,
        public_key: sig_data.public_key_b64url,
        signing_scheme: sig_data.signing_scheme,
    }
}

async fn run_onboard(
    rpc: String,
    party: Option<String>,
    private_key: Option<String>,
    invite_code: Option<String>,
    agent_name: Option<String>,
    email: Option<String>,
    env_file: PathBuf,
    poll_interval: u64,
    agent_toml_path: PathBuf,
) -> Result<()> {
    println!("=== Cloud Agent Self-Service Onboarding ===\n");

    // Step 1: Handle keys
    if let Some(ref pk_b58) = private_key {
        // --private-key provided (with --party): write key and derive public key
        let bytes = orderbook_agent_logic::config::decode_private_key(pk_b58)?;
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
        let bytes = orderbook_agent_logic::config::decode_private_key(&pk)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&bytes);
        let pub_b58 = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();
        upsert_env_value(&env_file, "PARTY_AGENT_PUBLIC_KEY", &pub_b58)?;
        println!("Public key: {}", pub_b58);
    } else {
        // No key provided and none in .env — generate new keypair
        println!("Generating new Ed25519 keypair...");
        let (priv_b58, pub_b58) = orderbook_agent_logic::sign::generate_keypair();
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
                let mut client = DAppProviderServiceClient::new(channel);
                let config_resp = client
                    .get_agent_config(GetAgentConfigRequest {})
                    .await
                    .context("GetAgentConfig RPC failed")?
                    .into_inner();
                write_server_config_to_env(&env_file, &rpc, &config_resp)?;
                println!("Server config written to {}", env_file.display());
            }
            println!("\nPARTY_AGENT={} — checking ledger onboarding...", party_id);
            return complete_ledger_onboarding(&env_file, &agent_toml_path, &rpc).await;
        }
    }

    // Need private key bytes and public key for the waiting list flow
    let pk = read_env_value(&env_file, "PARTY_AGENT_PRIVATE_KEY")
        .ok_or_else(|| anyhow::anyhow!("PARTY_AGENT_PRIVATE_KEY missing from .env"))?;
    let private_key_bytes = orderbook_agent_logic::config::decode_private_key(&pk)?;
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&private_key_bytes);
    let public_key_b58 = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();

    // Step 2: Connect to RPC (raw channel, no JWT auth needed)
    println!("\nConnecting to {}...", rpc);
    let channel = create_raw_channel(&rpc).await?;
    let mut client = DAppProviderServiceClient::new(channel);
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

    // Step 4: Register on waiting list (idempotent)
    println!("\nRegistering agent on waiting list...");
    let canonical = message_signing::canonical_register_agent(
        &public_key_b58,
        invite_code.as_deref(),
    );
    let sig = sign_onboarding_request(&private_key_bytes, &canonical);

    let register_resp = client
        .register_agent(RegisterAgentRequest {
            public_key: public_key_b58.clone(),
            invite_code,
            email,
            agent_name,
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
        let multihash_signature = orderbook_agent_logic::sign::sign_multihash(&signing_key, &multihash)?;

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

        println!("Status: SIGNATURE_REQUIRED — waiting for topology creation (polling every {}s)...", poll_interval);
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
    complete_ledger_onboarding(&env_file, &agent_toml_path, &rpc).await
}

/// Complete ledger onboarding (preapproval, user-service, subscription).
/// Called after PARTY_AGENT is set in .env.
async fn complete_ledger_onboarding(
    env_file: &std::path::Path,
    agent_toml_path: &std::path::Path,
    rpc: &str,
) -> Result<()> {
    // Ensure .env is loaded into process env
    let _ = dotenvy::from_path(env_file);

    // Create default agent.toml if missing
    if !agent_toml_path.exists() {
        let default_toml = r#"role = "agent"
auto_settle = true
poll_interval_secs = 10
token_ttl_secs = 3600
connection_timeout_secs = 30

[[markets]]
enabled = false
instrument_id = ""
instrument_admin = ""
"#;
        std::fs::write(agent_toml_path, default_toml)
            .context("Failed to create default agent.toml")?;
        println!("Created default {}", agent_toml_path.display());
    }

    let base_config = config::load(agent_toml_path)
        .context("Failed to load config (is .env complete?)")?;

    println!("\nCompleting ledger onboarding for party {}...", base_config.party_id);

    let mut client = DAppProviderClient::new(
        rpc,
        &base_config.party_id,
        &base_config.role,
        &base_config.private_key_bytes,
        base_config.token_ttl_secs,
        Some(base_config.node_name.as_str()),
        &base_config.ledger_service_public_key,
        Some(base_config.connection_timeout_secs),
    )
    .await
    .context("Failed to create authenticated DAppProvider client")?;

    // Template IDs for on-chain contract lookups
    const TMPL_PREAPPROVAL_PROPOSAL: &str =
        "#splice-wallet:Splice.Wallet.TransferPreapproval:TransferPreapprovalProposal";
    const TMPL_USER_SERVICE: &str =
        "#utility-settlement-app-v1:Utility.Settlement.App.V1.Service.User:UserService";
    const TMPL_USER_SERVICE_REQUEST: &str =
        "#utility-settlement-app-v1:Utility.Settlement.App.V1.Service.User:UserServiceRequest";

    // 11a: Request preapproval (check completed AND pending proposals)
    let preapprovals = client.get_preapprovals().await?;
    let proposals = client
        .get_active_contracts(&[TMPL_PREAPPROVAL_PROPOSAL.to_string()])
        .await
        .unwrap_or_default();
    if !preapprovals.is_empty() {
        println!("Preapproval already exists ({} found).", preapprovals.len());
    } else if !proposals.is_empty() {
        println!("Preapproval proposal pending ({} found), waiting for acceptance.", proposals.len());
    } else {
        println!("Requesting preapproval...");
        let expectation = OperationExpectation::RequestPreapproval {
            party: base_config.party_id.clone(),
        };
        client
            .submit_transaction(
                PrepareTransactionRequest {
                    operation: TransactionOperation::RequestPreapproval as i32,
                    params: Some(Params::RequestPreapproval(RequestPreapprovalParams {})),
                    request_signature: None,
                },
                &expectation,
                false,
                false,
                false,
            )
            .await
            .context("Failed to request preapproval")?;
        println!("Preapproval requested.");
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
            party: base_config.party_id.clone(),
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

    // 11c: Request pay-as-you-go subscription (check completed AND pending)
    let subscription_app = read_env_value(env_file, "SUBSCRIPTION_APP_PARTY")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| base_config.settlement_operator.clone());
    let subscription_amount = read_env_value(env_file, "SUBSCRIPTION_AMOUNT")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0.033".to_string());

    let recurring_pkg = read_env_value(env_file, "RECURRING_PAYMENT_PACKAGE_NAME")
        .filter(|s| !s.is_empty());

    if let Some(pkg) = &recurring_pkg {
        let payment_tmpl = format!("#{}:RecurringPayment:RecurringPayment", pkg);
        let request_tmpl = format!("#{}:RecurringPaymentRequest:RecurringPaymentRequest", pkg);
        let sub_contracts = client
            .get_active_contracts(&[payment_tmpl, request_tmpl])
            .await
            .unwrap_or_default();
        if sub_contracts.is_empty() {
            println!("Requesting pay-as-you-go subscription (app={}, amount={})...",
                subscription_app, subscription_amount);
            let expectation = OperationExpectation::RequestRecurringPayasyougo {
                party: base_config.party_id.clone(),
                app_party: subscription_app.clone(),
                amount: subscription_amount.clone(),
            };
            client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::RequestRecurringPayasyougo as i32,
                        params: Some(Params::RequestRecurringPayasyougo(
                            RequestRecurringPayasyougoParams {
                                app_party: subscription_app,
                                amount: subscription_amount,
                                description: Some("Agent subscription".to_string()),
                                reference: Some(uuid::Uuid::now_v7().to_string()),
                            },
                        )),
                        request_signature: None,
                    },
                    &expectation,
                    false,
                    false,
                    false,
                )
                .await
                .context("Failed to request pay-as-you-go subscription")?;
            println!("Subscription requested.");
        } else {
            println!("Subscription already exists or pending ({} contracts found).", sub_contracts.len());
        }
    } else {
        println!("RECURRING_PAYMENT_PACKAGE_NAME not configured, skipping subscription.");
    }

    println!("\n=== Onboarding complete! ===");
    println!("You can now run: cloud-agent agent");
    Ok(())
}

// ============================================================================
// User service commands
// ============================================================================

async fn run_user_service(config: BaseConfig, command: UserServiceCommands, verbose: bool, dry_run: bool, force: bool, confirm: bool) -> Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
    )
    .await?;

    match command {
        UserServiceCommands::Request { reference_id, party_name } => {
            if confirm && !dry_run {
                let lock = orderbook_agent_logic::confirm::new_confirm_lock();
                orderbook_agent_logic::confirm::confirm_transaction(
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

fn run_sign(config: BaseConfig, command: SignCommands) -> Result<()> {
    match command {
        SignCommands::Multihash { input, private_key } => {
            let key = orderbook_agent_logic::sign::resolve_signing_key(
                &config.private_key_bytes,
                private_key.as_deref(),
            )?;
            println!("{}", orderbook_agent_logic::sign::sign_multihash(&key, &input)?);
        }
        SignCommands::Message { input, private_key } => {
            let key = orderbook_agent_logic::sign::resolve_signing_key(
                &config.private_key_bytes,
                private_key.as_deref(),
            )?;
            println!("{}", orderbook_agent_logic::sign::sign_message(&key, &input));
        }
        SignCommands::Binary { input, private_key } => {
            let key = orderbook_agent_logic::sign::resolve_signing_key(
                &config.private_key_bytes,
                private_key.as_deref(),
            )?;
            println!("{}", orderbook_agent_logic::sign::sign_binary(&key, &input)?);
        }
    }
    Ok(())
}
