//! Base configuration for the orderbook agent
//!
//! Assembled from three sources:
//! 1. `.env` — shared infrastructure env vars (URLs, party IDs, secrets)
//! 2. `configuration.toml` — shared token config (registries, Canton Coin)
//! 3. `agent.toml` — agent-specific settings (markets, polling, JWT)
//!
//! This is the shared `BaseConfig` used by both the local agent and the cloud agent.
//! The local agent wraps this with a `Config` that adds ledger API URLs.

use anyhow::{anyhow, Context, Result};
use orderbook_proto::orderbook::Instrument;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::auth::get_public_key_hex;

// ============================================================================
// Agent TOML (agent.toml) — agent-specific settings only
// ============================================================================

/// Agent-specific TOML configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentToml {
    #[serde(default = "default_auto_settle")]
    auto_settle: bool,
    #[serde(default = "default_poll_interval_secs")]
    poll_interval_secs: u64,
    #[serde(default = "default_role")]
    role: String,
    #[serde(default = "default_token_ttl_secs")]
    token_ttl_secs: u64,
    #[serde(default = "default_connection_timeout_secs")]
    connection_timeout_secs: u64,
    #[serde(default = "default_request_timeout_secs")]
    request_timeout_secs: u64,
    #[serde(default = "default_canton_op_timeout_secs")]
    canton_op_timeout_secs: u64,
    #[serde(default)]
    markets: Vec<MarketConfig>,
    /// LP configuration (only for liquidity provider agents)
    #[serde(default)]
    liquidity_provider: Option<LiquidityProviderConfig>,
}

// ============================================================================
// Shared token config (configuration.toml)
// ============================================================================

/// Shared token configuration from configuration.toml
#[derive(Debug, Clone, Deserialize)]
struct SharedConfiguration {
    #[serde(default)]
    registry: Vec<RegistryConfig>,
    #[serde(default)]
    canton_coin: Vec<CantonCoinConfig>,
    #[serde(default)]
    instrument: Vec<InstrumentConfig>,
    #[serde(default)]
    ledger_interfaces: Option<LedgerInterfacesConfig>,
}

/// Optional `[ledger_interfaces]` section in `configuration.toml` consumed by
/// `orderbook-ledger-service`. Selects which functional groups of the
/// DAppProviderService are exposed to clients (DVP, transfers, CIP-56, etc.).
///
/// If the section is omitted, all interfaces are enabled (backward compatible).
///
/// Example:
/// ```toml
/// [ledger_interfaces]
/// enabled = ["core", "transfer", "preapproval", "cip56", "user_service"]
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct LedgerInterfacesConfig {
    #[serde(default)]
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryConfig {
    party: String,
    #[allow(dead_code)]
    description: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CantonCoinConfig {
    token_id: String,
    dso_party: String,
    #[allow(dead_code)]
    description: String,
}

/// Instrument-to-registry mapping from configuration.toml
#[derive(Debug, Clone, Deserialize)]
struct InstrumentConfig {
    id: String,
    registry: String,
}

// ============================================================================
// BaseConfig (shared between local agent and cloud agent)
// ============================================================================

/// Base agent configuration (no ledger API URLs)
///
/// Contains everything needed for settlement orchestration, order tracking,
/// and authentication. The local agent wraps this with additional ledger URLs.
#[derive(Debug, Clone)]
pub struct BaseConfig {
    // From .env
    pub orderbook_grpc_url: String,
    pub synchronizer_id: String,
    pub party_id: String,
    pub private_key_bytes: [u8; 32],
    pub private_key_base58: String,
    pub public_key_hex: String,
    pub settlement_operator: String,
    pub fee_reserve_cc: f64,
    // agent_fee_cc / participant_fee_cc / signature_fee_cc moved to
    // ledger-service (`LedgerServiceConfig`). The ledger now issues these
    // fees as a server-signed schedule and debits them off-chain.
    //
    // PARTY_TRAFFIC_FEE / TRAFFIC_FEE_PRICE_USD_MB / JOIN_TRAFFIC_TRANSACTIONS
    // also moved off-chain: traffic billing is now handled entirely by the
    // ledger via the prepaid traffic pool (`PARTY_PREPAID_TRAFFIC` on the
    // ledger side).
    pub merge_threshold: Option<usize>,
    pub merge_max_amulets: usize,
    pub merge_poll_interval_sec: u64,
    pub settlement_thread_count: usize,

    pub dso_party: String,

    // From configuration.toml
    pub onboarded_registries: Vec<String>,
    pub cc_token_id: Option<String>,
    /// Instrument ID → registry party mapping (from [[instrument]] + [[canton_coin]])
    pub instrument_registries: HashMap<String, String>,

    // From agent.toml
    pub auto_settle: bool,
    pub poll_interval_secs: u64,
    pub role: String,
    pub token_ttl_secs: u64,
    pub connection_timeout_secs: u64,
    pub request_timeout_secs: u64,
    /// Backstop timeout for any single Canton-touching await in the runner main
    /// loop (per RPC / per `update_cycle` etc). Generous by design — Canton
    /// blockchain ops can legitimately take minutes. The point is that it is
    /// finite, so a stuck connection cannot hang shutdown forever.
    pub canton_op_timeout_secs: u64,
    pub markets: Vec<MarketConfig>,

    // Multi-node routing
    pub node_name: String,

    // Message signing
    pub ledger_service_public_key: [u8; 32],

    // Liquidity provider (LP agents only)
    pub liquidity_provider: Option<LiquidityProviderConfig>,

    // Settlement throttle
    pub max_active_settlements: usize,

    // Settlement expiry (max lifetime before considering expired)
    pub settle_before_secs: u64,

    // Allocation expiry (max age before an un-allocated settlement is considered
    // expired and its liquidity reservation released)
    pub allocate_before_secs: u64,

    // Liquidity management
    /// Safety margin multiplier for fee estimates (e.g. 1.1 = 10%)
    pub liquidity_margin: f64,
    /// EMA window in hours for flow depletion tracking
    pub flow_ema_window_hours: f64,
    /// Hours to depletion at which spread coefficient = 0
    pub depletion_max_hours: f64,
    /// Hours to depletion at which spread coefficient = 10
    pub depletion_min_hours: f64,

    // Auto top-up of the off-chain prepaid traffic balance.
    // Both must be set together. May be negative if the agent's party has
    // a credit_limit_cc in canton-agent's `party_credit_limits` table
    // (balance is allowed to go negative down to -credit_limit_cc).
    /// Minimum prepaid traffic balance in CC; below this the agent tops up.
    pub min_prepaid_traffic_balance_cc: Option<rust_decimal::Decimal>,
    /// Amount to credit on each top-up, in CC.
    pub prepaid_traffic_topup_cc: Option<rust_decimal::Decimal>,
}

#[cfg(test)]
impl BaseConfig {
    /// Minimal config for unit tests. Only the fields a test actually reads
    /// matter; everything else is a zero/empty placeholder. Centralized here so
    /// adding a `BaseConfig` field breaks one place, not every executor test.
    pub(crate) fn test_minimal() -> Self {
        Self {
            orderbook_grpc_url: String::new(),
            synchronizer_id: String::new(),
            party_id: "test-party".to_string(),
            private_key_bytes: [0u8; 32],
            private_key_base58: String::new(),
            public_key_hex: String::new(),
            settlement_operator: String::new(),
            fee_reserve_cc: 5.0,
            merge_threshold: None,
            merge_max_amulets: 0,
            merge_poll_interval_sec: 0,
            settlement_thread_count: 1,
            dso_party: String::new(),
            onboarded_registries: Vec::new(),
            cc_token_id: None,
            instrument_registries: HashMap::new(),
            auto_settle: false,
            poll_interval_secs: 0,
            role: "agent".to_string(),
            token_ttl_secs: 60,
            connection_timeout_secs: 10,
            request_timeout_secs: 10,
            canton_op_timeout_secs: 60,
            markets: Vec::new(),
            node_name: String::new(),
            ledger_service_public_key: [0u8; 32],
            liquidity_provider: None,
            max_active_settlements: 1000,
            settle_before_secs: 1800,
            allocate_before_secs: 900,
            liquidity_margin: 1.1,
            flow_ema_window_hours: 4.0,
            depletion_max_hours: 12.0,
            depletion_min_hours: 1.0,
            min_prepaid_traffic_balance_cc: None,
            prepaid_traffic_topup_cc: None,
        }
    }
}

impl BaseConfig {
    /// Strict loader — fails if `agent.toml` is missing or unparseable.
    /// Use from commands that actually consume market/LP settings (i.e. `agent`).
    pub fn load<P: AsRef<Path>>(agent_toml_path: P) -> Result<Self> {
        let agent_toml_str = fs::read_to_string(agent_toml_path.as_ref())
            .with_context(|| format!("Failed to read {}", agent_toml_path.as_ref().display()))?;
        let agent: AgentToml = toml::from_str(&agent_toml_str)
            .with_context(|| format!("Failed to parse {}", agent_toml_path.as_ref().display()))?;
        Self::assemble(agent)
    }

    /// Lenient loader — if `agent.toml` is missing, use serde defaults
    /// (empty markets, no LP, default timeouts). Still fails if the file
    /// exists but is malformed, and still requires the mandatory env vars.
    /// Use from commands that don't need market/LP config (faucet, transfer, etc.).
    pub fn load_or_defaults<P: AsRef<Path>>(agent_toml_path: P) -> Result<Self> {
        let agent: AgentToml = if agent_toml_path.as_ref().exists() {
            let s = fs::read_to_string(agent_toml_path.as_ref())
                .with_context(|| format!("Failed to read {}", agent_toml_path.as_ref().display()))?;
            toml::from_str(&s)
                .with_context(|| format!("Failed to parse {}", agent_toml_path.as_ref().display()))?
        } else {
            // Empty string round-trips to AgentToml with all serde defaults.
            toml::from_str("").expect("AgentToml serde defaults must parse")
        };
        Self::assemble(agent)
    }

    /// Assemble the full BaseConfig from an already-parsed AgentToml +
    /// env vars. Instrument/registry info is populated later by
    /// [`BaseConfig::populate_instruments_from_rpc`]. Shared between
    /// strict/lenient loaders.
    fn assemble(agent: AgentToml) -> Result<Self> {
        // Instrument registry placeholders — filled by populate_instruments_from_rpc
        // after the cloud-agent fetches them over gRPC at startup.
        let onboarded_registries: Vec<String> = Vec::new();
        let cc_token_id: Option<String> = None;
        let instrument_registries: HashMap<String, String> = HashMap::new();

        // Read env vars
        let dso_party = std::env::var("DSO")
            .map_err(|_| anyhow!("DSO env var is required"))?;

        let party_id = std::env::var("PARTY_AGENT")
            .map_err(|_| anyhow!("PARTY_AGENT env var is required"))?;

        let private_key_base58 = std::env::var("PARTY_AGENT_PRIVATE_KEY")
            .map_err(|_| anyhow!("PARTY_AGENT_PRIVATE_KEY env var is required"))?;

        let private_key_bytes = decode_private_key(&private_key_base58)?;
        let public_key_hex = get_public_key_hex(&private_key_bytes);

        let orderbook_grpc_url = std::env::var("ORDERBOOK_GRPC_URL")
            .map_err(|_| anyhow!("ORDERBOOK_GRPC_URL env var is required"))?;

        let synchronizer_id = std::env::var("SYNCHRONIZER_ID")
            .map_err(|_| anyhow!("SYNCHRONIZER_ID env var is required"))?;

        let settlement_operator = std::env::var("PARTY_SETTLEMENT_OPERATOR")
            .map_err(|_| anyhow!("PARTY_SETTLEMENT_OPERATOR env var is required"))?;

        // PARTY_ORDERBOOK_FEE is no longer read by the cloud-agent: every
        // fee that previously flowed through the on-chain BatchPay → fee
        // party path is now debited off-chain by the ledger via
        // `record_authorized_debits`, and the orderbook-server's Pass B
        // drains PARTY_PREPAID_TRAFFIC → PARTY_ORDERBOOK_FEE periodically.
        //
        // PARTY_TRAFFIC_FEE / TRAFFIC_FEE_PRICE_USD_MB / JOIN_TRAFFIC_TRANSACTIONS
        // are also no longer read by the cloud-agent: traffic billing is
        // handled off-chain by the ledger via the prepaid pool.

        let fee_reserve_cc = std::env::var("AGENT_FEE_RESERVE_CC")
            .unwrap_or_else(|_| "5.0".to_string())
            .parse::<f64>()
            .unwrap_or(5.0);

        // AGENT_FEE_CC / PARTICIPANT_FEE_CC / SIGNATURE_FEE_CC have moved to
        // the ledger-service config (`LedgerServiceConfig`). The ledger
        // issues these fees as a server-signed schedule that the cloud-agent
        // authorizes via `fees_authorization` on ExecuteTransactionRequest.

        let merge_threshold = std::env::var("MERGE_THRESHOLD").ok().and_then(|v| v.parse().ok());
        let merge_max_amulets = std::env::var("MERGE_MAX_AMULETS").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
        let merge_poll_interval_sec = std::env::var("MERGE_POLL_INTERVAL_SEC").ok().and_then(|v| v.parse().ok()).unwrap_or(600);

        let settlement_thread_count = std::env::var("SETTLEMENT_THREAD_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(25);

        let max_active_settlements = std::env::var("AGENT_MAX_SETTLEMENTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        let node_name = std::env::var("NODE_NAME")
            .map_err(|_| anyhow!("NODE_NAME env var is required"))?;

        let ledger_service_public_key_b58 = std::env::var("LEDGER_SERVICE_PUBLIC_KEY")
            .map_err(|_| anyhow!("LEDGER_SERVICE_PUBLIC_KEY env var is required"))?;
        let ledger_service_public_key = decode_public_key(&ledger_service_public_key_b58)?;

        // The quoted windows are stamped into the on-chain DVP terms, where the
        // DAML model requires 0 < allocateBefore < settleBefore — a misordered
        // market would permanently fail every DVP propose on that pair, so
        // refuse to start instead.
        for market in &agent.markets {
            if let Some(rfq) = &market.rfq {
                if rfq.allocate_before_secs == 0
                    || rfq.allocate_before_secs >= rfq.settle_before_secs
                {
                    return Err(anyhow!(
                        "Market {}: invalid [markets.rfq] deadline windows: \
                         allocate_before_secs={} settle_before_secs={} \
                         (need 0 < allocate_before_secs < settle_before_secs)",
                        market.market_id, rfq.allocate_before_secs, rfq.settle_before_secs
                    ));
                }
            }
        }

        let settle_before_secs = agent.markets.iter()
            .filter_map(|m| m.rfq.as_ref())
            .map(|r| r.settle_before_secs as u64)
            .max()
            .unwrap_or(default_rfq_settle_before_secs() as u64);

        let allocate_before_secs = agent.markets.iter()
            .filter_map(|m| m.rfq.as_ref())
            .map(|r| r.allocate_before_secs as u64)
            .max()
            .unwrap_or(default_rfq_allocate_before_secs() as u64);

        let liquidity_margin = std::env::var("LIQUIDITY_MARGIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.1);

        let flow_ema_window_hours = std::env::var("FLOW_EMA_WINDOW_HOURS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4.0);

        let depletion_max_hours = std::env::var("DEPLETION_COEFF_MAX_HOURS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(12.0);

        let depletion_min_hours = std::env::var("DEPLETION_COEFF_MIN_HOURS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);

        // Auto-topup vars: both must be set together. If exactly one is set,
        // fail at startup so the operator gets a clear signal rather than
        // silently disabling the feature.
        let min_prepaid = std::env::var("MIN_PREPAID_TRAFFIC_BALANCE_CC").ok();
        let topup_prepaid = std::env::var("PREPAID_TRAFFIC_TOPUP_CC").ok();
        let (min_prepaid_traffic_balance_cc, prepaid_traffic_topup_cc) =
            match (min_prepaid, topup_prepaid) {
                (Some(min_str), Some(topup_str)) => {
                    let min: rust_decimal::Decimal = min_str.parse()
                        .with_context(|| format!("MIN_PREPAID_TRAFFIC_BALANCE_CC must be a decimal, got '{}'", min_str))?;
                    let topup: rust_decimal::Decimal = topup_str.parse()
                        .with_context(|| format!("PREPAID_TRAFFIC_TOPUP_CC must be a decimal, got '{}'", topup_str))?;
                    if topup <= rust_decimal::Decimal::ZERO {
                        return Err(anyhow!(
                            "PREPAID_TRAFFIC_TOPUP_CC must be > 0, got {}",
                            topup
                        ));
                    }
                    (Some(min), Some(topup))
                }
                (None, None) => (None, None),
                (Some(_), None) => return Err(anyhow!(
                    "MIN_PREPAID_TRAFFIC_BALANCE_CC is set but PREPAID_TRAFFIC_TOPUP_CC is not — both required for auto-topup"
                )),
                (None, Some(_)) => return Err(anyhow!(
                    "PREPAID_TRAFFIC_TOPUP_CC is set but MIN_PREPAID_TRAFFIC_BALANCE_CC is not — both required for auto-topup"
                )),
            };

        Ok(BaseConfig {
            orderbook_grpc_url,
            synchronizer_id,
            party_id,
            private_key_bytes,
            private_key_base58,
            public_key_hex,
            settlement_operator,
            fee_reserve_cc,
            merge_threshold,
            merge_max_amulets,
            merge_poll_interval_sec,
            settlement_thread_count,
            dso_party,
            onboarded_registries,
            cc_token_id,
            instrument_registries,
            auto_settle: agent.auto_settle,
            poll_interval_secs: agent.poll_interval_secs,
            role: agent.role,
            token_ttl_secs: agent.token_ttl_secs,
            connection_timeout_secs: agent.connection_timeout_secs,
            request_timeout_secs: agent.request_timeout_secs,
            canton_op_timeout_secs: agent.canton_op_timeout_secs,
            markets: agent.markets,
            node_name,
            ledger_service_public_key,
            liquidity_provider: agent.liquidity_provider,
            max_active_settlements,
            settle_before_secs,
            allocate_before_secs,
            liquidity_margin,
            flow_ema_window_hours,
            depletion_max_hours,
            depletion_min_hours,
            min_prepaid_traffic_balance_cc,
            prepaid_traffic_topup_cc,
        })
    }

    /// Get list of enabled markets
    pub fn enabled_markets(&self) -> Vec<&MarketConfig> {
        self.markets.iter().filter(|m| m.enabled).collect()
    }

    /// Populate `cc_token_id`, `onboarded_registries` and `instrument_registries`
    /// from instruments fetched over the orderbook-rpc `GetInstruments` endpoint.
    /// Call this once at startup before running settlement / fill / transfer logic
    /// that uses [`BaseConfig::resolve_instrument`].
    ///
    /// The Canton Coin instrument is identified by `instrument_type == "token"`;
    /// its `instrument_id` drives the CC → Amulet translation and its `registry`
    /// is the DSO party. Every other instrument simply maps id → registry.
    pub fn populate_instruments_from_rpc(&mut self, instruments: Vec<Instrument>) {
        let mut instrument_registries: HashMap<String, String> = HashMap::new();
        let mut cc_token_id: Option<String> = None;
        let mut onboarded_registries: HashSet<String> = HashSet::new();

        for inst in instruments {
            let registry = inst.registry.clone().unwrap_or_default();
            if !registry.is_empty() {
                instrument_registries.insert(inst.instrument_id.clone(), registry.clone());
                onboarded_registries.insert(registry);
            }
            if inst.instrument_type == "token" && cc_token_id.is_none() {
                cc_token_id = Some(inst.instrument_id.clone());
            }
        }

        self.cc_token_id = cc_token_id;
        self.instrument_registries = instrument_registries;
        self.onboarded_registries = onboarded_registries.into_iter().collect();
        tracing::info!(
            "Instrument registry populated from RPC: {} instruments, {} registries, cc_token_id={:?}",
            self.instrument_registries.len(),
            self.onboarded_registries.len(),
            self.cc_token_id,
        );
    }

    /// Resolve an orderbook instrument ID to (on_chain_id, registry_party).
    ///
    /// CC maps to on-chain "Amulet"; other instruments use their ID as-is.
    /// Registry comes from instruments fetched via `populate_instruments_from_rpc`.
    /// Returns empty strings if instrument not found (verification skipped).
    pub fn resolve_instrument(&self, instrument_id: &str) -> (String, String) {
        let on_chain_id = if Some(instrument_id) == self.cc_token_id.as_deref() {
            "Amulet".to_string()
        } else {
            instrument_id.to_string()
        };
        let registry = self
            .instrument_registries
            .get(instrument_id)
            .cloned()
            .unwrap_or_default();
        (on_chain_id, registry)
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Decode base58 Ed25519 public key to 32 bytes
pub fn decode_public_key(base58_key: &str) -> Result<[u8; 32]> {
    let key_bytes = bs58::decode(base58_key)
        .into_vec()
        .context("Invalid base58 public key")?;

    if key_bytes.len() != 32 {
        anyhow::bail!(
            "Public key must be exactly 32 bytes, got {}",
            key_bytes.len()
        );
    }

    let mut arr = [0u8; 32];
    arr.copy_from_slice(&key_bytes);
    Ok(arr)
}

/// Decode base58 Ed25519 private key to 32-byte seed
pub fn decode_private_key(base58_key: &str) -> Result<[u8; 32]> {
    let key_bytes = bs58::decode(base58_key)
        .into_vec()
        .context("Invalid base58 private key")?;

    if key_bytes.len() < 32 {
        anyhow::bail!(
            "Private key too short: expected at least 32 bytes, got {}",
            key_bytes.len()
        );
    }

    let mut arr = [0u8; 32];
    arr.copy_from_slice(&key_bytes[..32]);
    Ok(arr)
}

/// Load the optional `[ledger_interfaces]` section from `configuration.toml`.
///
/// Returns:
/// - `Ok(None)` — file does not exist (legitimate default; ledger-service
///   treats this as "all interfaces enabled"), or the file parses cleanly
///   but omits the `[ledger_interfaces]` section.
/// - `Ok(Some(cfg))` — file parses cleanly and contains the section.
/// - `Err(...)` — file exists but TOML parsing failed.
///
/// **Fail-loud on parse errors is intentional**: this section drives security-
/// relevant interface gating. A typo like `[ledger_inerfaces]` or any other
/// malformed TOML would otherwise silently disable all gating and let every
/// interface through. Callers MUST propagate the error so startup aborts.
pub fn load_ledger_interfaces(path: &str) -> Result<Option<LedgerInterfacesConfig>> {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(None), // Missing file = no gating (legitimate default)
    };
    let config: SharedConfiguration = toml::from_str(&contents).with_context(|| {
        format!(
            "Failed to parse {} while loading [ledger_interfaces] section",
            path
        )
    })?;
    Ok(config.ledger_interfaces)
}

/// Load shared configuration.toml (registries + canton_coin + instruments)
///
/// Returns (onboarded_registries, cc_token_id, instrument_registries).
/// Used by both agents and the ledger service.
pub fn load_shared_configuration(
    path: &str,
) -> (Vec<String>, Option<String>, HashMap<String, String>) {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!("{} not found, continuing without registry filtering", path);
            return (Vec::new(), None, HashMap::new());
        }
    };

    match toml::from_str::<SharedConfiguration>(&contents) {
        Ok(config) => {
            tracing::info!(
                "Loaded {} registries from {}",
                config.registry.len(),
                path
            );
            let registries = config.registry.into_iter().map(|r| r.party).collect();
            let cc_id = config.canton_coin.into_iter().next().map(|cc| {
                tracing::info!("Canton Coin: {} ({})", cc.token_id, cc.dso_party);
                cc.token_id
            });

            // Build instrument → registry map from [[instrument]] entries
            let mut instrument_registries: HashMap<String, String> = config
                .instrument
                .into_iter()
                .map(|i| (i.id, i.registry))
                .collect();
            // Add CC → DSO automatically (DSO comes from env var at load time)
            if let Some(ref id) = cc_id {
                if let Ok(dso) = std::env::var("DSO") {
                    instrument_registries.insert(id.clone(), dso);
                }
            }
            if !instrument_registries.is_empty() {
                tracing::info!(
                    "Loaded {} instrument→registry mappings",
                    instrument_registries.len()
                );
            }

            (registries, cc_id, instrument_registries)
        }
        Err(e) => {
            tracing::warn!("Failed to parse {}: {}", path, e);
            (Vec::new(), None, HashMap::new())
        }
    }
}

// ============================================================================
// Market config types
// ============================================================================

/// Configuration for a single market (optional, for order placement)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketConfig {
    pub market_id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub base_order_size: Option<String>,
    #[serde(default)]
    pub bid_levels: Vec<PriceLevel>,
    #[serde(default)]
    pub offer_levels: Vec<PriceLevel>,
    #[serde(default = "default_price_change_threshold")]
    pub price_change_threshold_percent: f64,
    /// RFQ configuration for this market (LP agents only)
    #[serde(default)]
    pub rfq: Option<RfqMarketConfig>,
}

/// A single price level in the grid
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub delta_percent: f64,
    pub quantity: String,
}

/// RFQ market configuration for LP agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqMarketConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub min_quantity: String,
    pub max_quantity: String,
    #[serde(default = "default_rfq_spread")]
    pub bid_spread_percent: f64,
    #[serde(default = "default_rfq_spread")]
    pub offer_spread_percent: f64,
    #[serde(default)]
    pub quote_valid_secs: Option<u32>,
    /// DVP allocation deadline in seconds from DVP creation (default 15 minutes)
    #[serde(default = "default_rfq_allocate_before_secs")]
    pub allocate_before_secs: u32,
    /// DVP settlement deadline in seconds from DVP creation (default 30 minutes)
    #[serde(default = "default_rfq_settle_before_secs")]
    pub settle_before_secs: u32,
    /// Per-market override of the LP's global `min_notional_usd` (USD).
    /// When set, takes precedence over `[liquidity_provider].min_notional_usd`.
    #[serde(default)]
    pub min_notional_usd: Option<f64>,
}

/// Liquidity provider configuration (LP agents only)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityProviderConfig {
    pub name: String,
    #[serde(default = "default_max_concurrent_rfqs")]
    pub max_concurrent_rfqs: usize,
    #[serde(default = "default_quote_valid_secs")]
    pub default_quote_valid_secs: u32,
    /// Global minimum RFQ value in USD. RFQs whose USD notional is below this
    /// are rejected (AmountTooSmall). 0 = disabled. Overridden per-market by
    /// `[markets.rfq].min_notional_usd`.
    #[serde(default)]
    pub min_notional_usd: f64,
}

// ============================================================================
// Defaults
// ============================================================================

fn default_auto_settle() -> bool {
    true
}

fn default_poll_interval_secs() -> u64 {
    5
}

fn default_role() -> String {
    "trader".to_string()
}

fn default_token_ttl_secs() -> u64 {
    3600
}

fn default_connection_timeout_secs() -> u64 {
    30
}

fn default_request_timeout_secs() -> u64 {
    120
}

/// 10 minutes — much higher than 120s to accommodate slow Canton txs, but
/// still finite so a dead gRPC connection cannot trap the runner forever.
fn default_canton_op_timeout_secs() -> u64 {
    600
}

fn default_enabled() -> bool {
    true
}

fn default_price_change_threshold() -> f64 {
    0.5
}

fn default_rfq_spread() -> f64 {
    0.5
}

fn default_max_concurrent_rfqs() -> usize {
    10
}

fn default_quote_valid_secs() -> u32 {
    30
}

fn default_rfq_allocate_before_secs() -> u32 {
    900 // 15 minutes
}

fn default_rfq_settle_before_secs() -> u32 {
    1800 // 30 minutes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_private_key() {
        let base58 = "EB92Q6V2a78t9ppqMuKLppyfzFgyYJciQEVHZKnXAhjEwVpx9aMbQN84SR4ceo3mbLUxQF7TLzaEujaTJnS7eRF";
        let bytes = decode_private_key(base58).unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn test_agent_toml_defaults() {
        let toml_str = "";
        let agent: AgentToml = toml::from_str(toml_str).unwrap();
        assert!(agent.auto_settle);
        assert_eq!(agent.poll_interval_secs, 5);
        assert_eq!(agent.role, "trader");
        assert_eq!(agent.token_ttl_secs, 3600);
        assert_eq!(agent.connection_timeout_secs, 30);
        assert_eq!(agent.request_timeout_secs, 120);
        assert_eq!(agent.canton_op_timeout_secs, 600);
        assert!(agent.markets.is_empty());
    }

    #[test]
    fn test_min_notional_usd_config() {
        // Omitted → global default 0.0 (disabled) and per-market None (no override).
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"

[[markets]]
market_id = "CC-USDCx"

[markets.rfq]
min_quantity = "5"
max_quantity = "1000"
"#,
        )
        .unwrap();
        assert_eq!(agent.liquidity_provider.as_ref().unwrap().min_notional_usd, 0.0);
        assert_eq!(agent.markets[0].rfq.as_ref().unwrap().min_notional_usd, None);

        // Global set with an integer literal (as written in the deployed tomls)
        // must coerce to f64; the per-market value overrides the global.
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"
min_notional_usd = 10

[[markets]]
market_id = "CC-USDCx"

[markets.rfq]
min_quantity = "5"
max_quantity = "1000"
min_notional_usd = 25.0
"#,
        )
        .unwrap();
        assert_eq!(agent.liquidity_provider.as_ref().unwrap().min_notional_usd, 10.0);
        assert_eq!(agent.markets[0].rfq.as_ref().unwrap().min_notional_usd, Some(25.0));
    }
}
