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
use serde::{Deserialize, Serialize};
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
    pub fee_party: String,
    pub traffic_fee_party: String,
    pub traffic_fee_usd_per_byte: f64,
    pub join_traffic: bool,
    pub fee_reserve_cc: f64,
    pub settlement_thread_count: usize,

    // From configuration.toml
    pub onboarded_registries: Vec<String>,
    pub cc_dso_party: Option<String>,
    pub cc_token_id: Option<String>,

    // From agent.toml
    pub auto_settle: bool,
    pub poll_interval_secs: u64,
    pub role: String,
    pub token_ttl_secs: u64,
    pub connection_timeout_secs: u64,
    pub markets: Vec<MarketConfig>,

    // Multi-node routing
    pub node_name: String,

    // Message signing
    pub ledger_service_public_key: [u8; 32],

    // Liquidity provider (LP agents only)
    pub liquidity_provider: Option<LiquidityProviderConfig>,

    // Settlement throttle
    pub max_active_settlements: usize,
}

impl BaseConfig {
    /// Load configuration from .env + configuration.toml + agent.toml
    pub fn load<P: AsRef<Path>>(agent_toml_path: P) -> Result<Self> {
        // 1. Read agent.toml
        let agent_toml_str = fs::read_to_string(agent_toml_path.as_ref())
            .with_context(|| format!("Failed to read {}", agent_toml_path.as_ref().display()))?;
        let agent: AgentToml = toml::from_str(&agent_toml_str)
            .with_context(|| format!("Failed to parse {}", agent_toml_path.as_ref().display()))?;

        // 2. Read configuration.toml (optional, graceful fallback)
        let (onboarded_registries, cc_dso_party, cc_token_id) =
            load_shared_configuration("configuration.toml");

        // 3. Read env vars
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

        let traffic_fee_party = std::env::var("PARTY_TRAFFIC_FEE")
            .map_err(|_| anyhow!("PARTY_TRAFFIC_FEE env var is required"))?;

        let traffic_fee_usd_per_byte = std::env::var("TRAFFIC_FEE_PRICE_USD_MB")
            .map_err(|_| anyhow!("TRAFFIC_FEE_PRICE_USD_MB env var is required"))?
            .parse::<f64>()
            .map_err(|_| anyhow!("TRAFFIC_FEE_PRICE_USD_MB must be a valid number"))?
            / 1_000_000.0;

        let fee_party = std::env::var("PARTY_ORDERBOOK_FEE")
            .map_err(|_| anyhow!("PARTY_ORDERBOOK_FEE env var is required"))?;
        let join_traffic = std::env::var("JOIN_TRAFFIC_TRANSACTIONS")
            .map(|v| v != "false")
            .unwrap_or(true);

        let fee_reserve_cc = std::env::var("AGENT_FEE_RESERVE_CC")
            .unwrap_or_else(|_| "5.0".to_string())
            .parse::<f64>()
            .unwrap_or(5.0);

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

        Ok(BaseConfig {
            orderbook_grpc_url,
            synchronizer_id,
            party_id,
            private_key_bytes,
            private_key_base58,
            public_key_hex,
            settlement_operator,
            fee_party,
            traffic_fee_party,
            traffic_fee_usd_per_byte,
            join_traffic,
            fee_reserve_cc,
            settlement_thread_count,
            onboarded_registries,
            cc_dso_party,
            cc_token_id,
            auto_settle: agent.auto_settle,
            poll_interval_secs: agent.poll_interval_secs,
            role: agent.role,
            token_ttl_secs: agent.token_ttl_secs,
            connection_timeout_secs: agent.connection_timeout_secs,
            markets: agent.markets,
            node_name,
            ledger_service_public_key,
            liquidity_provider: agent.liquidity_provider,
            max_active_settlements,
        })
    }

    /// Get list of enabled markets
    pub fn enabled_markets(&self) -> Vec<&MarketConfig> {
        self.markets.iter().filter(|m| m.enabled).collect()
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

/// Load shared configuration.toml (registries + canton_coin)
///
/// Returns (onboarded_registries, cc_dso_party, cc_token_id).
/// Used by both agents and the ledger service.
pub fn load_shared_configuration(
    path: &str,
) -> (Vec<String>, Option<String>, Option<String>) {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!("{} not found, continuing without registry filtering", path);
            return (Vec::new(), None, None);
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
            let (cc_dso, cc_id) = config.canton_coin.into_iter().next().map_or(
                (None, None),
                |cc| {
                    tracing::info!("Canton Coin: {} ({})", cc.token_id, cc.dso_party);
                    (Some(cc.dso_party), Some(cc.token_id))
                },
            );
            (registries, cc_dso, cc_id)
        }
        Err(e) => {
            tracing::warn!("Failed to parse {}: {}", path, e);
            (Vec::new(), None, None)
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
    /// DVP allocation deadline in seconds from DVP creation (default 1 hour)
    #[serde(default = "default_rfq_allocate_before_secs")]
    pub allocate_before_secs: u32,
    /// DVP settlement deadline in seconds from DVP creation (default 2 hours)
    #[serde(default = "default_rfq_settle_before_secs")]
    pub settle_before_secs: u32,
}

/// Liquidity provider configuration (LP agents only)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityProviderConfig {
    pub name: String,
    #[serde(default = "default_max_concurrent_rfqs")]
    pub max_concurrent_rfqs: usize,
    #[serde(default = "default_quote_valid_secs")]
    pub default_quote_valid_secs: u32,
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
    3600 // 1 hour
}

fn default_rfq_settle_before_secs() -> u32 {
    7200 // 2 hours
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
        assert!(agent.markets.is_empty());
    }
}
