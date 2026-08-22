//! Base configuration for the orderbook agent
//!
//! Assembled from three sources:
//! 1. `.env` — shared infrastructure env vars (URLs, party IDs, secrets)
//! 2. `configuration.toml` — shared token config (registries, Canton Coin)
//! 3. `agent.toml` — agent-specific settings (markets, polling, JWT)
//!
//! This is the shared `BaseConfig` used by both the local agent and the cloud agent.
//! The local agent wraps this with a `Config` that adds ledger API URLs.

use anyhow::{Context, Result, anyhow};
use orderbook_proto::orderbook::Instrument;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use zeroize::Zeroize;

use crate::auth::get_public_key_hex;
use crate::secret::{Secret, Zeroizing};

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
    /// Venue/branch-scoped overrides of `[markets.rfq]` params (RFQ V2 only).
    #[serde(default)]
    venue_overrides: Vec<VenueOverride>,
    /// LP configuration (only for liquidity provider agents)
    #[serde(default)]
    liquidity_provider: Option<LiquidityProviderConfig>,
    /// Force RFQ V2 (AtomicDVP) only: never connect the V1 LP settlement
    /// stream (no V1 LP registration, no V1 quotes) and never place
    /// grid/limit orders. Requires `[liquidity_provider.rfq_v2].enabled` and
    /// at least one enabled market with `[markets.rfq.v2].enabled`.
    /// Env override: RFQ_V2_ONLY.
    #[serde(default)]
    rfq_v2_only: bool,
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
    /// Ed25519 signing seed, stored sealed; decrypt via `.expose()`.
    pub private_key: Secret<32>,
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
    /// Instrument ID → ON-CHAIN wire id (`instruments.symbol`). Identical to
    /// the id for legacy tokens; diverges when an issuer mints an instrument
    /// under an opaque id (e.g. a UUID). Populated by `populate_instruments_from_rpc`.
    pub instrument_wire_ids: HashMap<String, String>,

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

    /// Venue/branch-scoped overrides of `[markets.rfq]` params (RFQ V2 only) —
    /// TOML `[[venue_overrides]]`, resolved per request by
    /// [`resolve_rfq_config`].
    pub venue_overrides: Vec<VenueOverride>,

    // Multi-node routing
    pub node_name: String,

    /// RFQ V2 swap-venue
    /// (^[a-z0-9][a-z0-9-]{1,19}$); it never affects auth, fees, the on-chain
    /// quote_id prefix, or the venue name.
    pub venue_branch: Option<String>,

    // Message signing
    pub ledger_service_public_key: [u8; 32],

    // Liquidity provider (LP agents only)
    pub liquidity_provider: Option<LiquidityProviderConfig>,

    /// RFQ V2 (AtomicDVP) only mode: the V1 LP settlement stream is never
    /// opened (no V1 LP registration/quotes) and grid/limit orders are never
    /// placed. The startup cancel-all still runs (clears any existing grid).
    /// In-flight V1 settlements still needing this agent's steps are ACTIVELY
    /// CANCELLED on encounter (see `abort_v1_settlement`) — they could never
    /// complete anyway, since the server's fee gate keys "agent" off live
    /// V1-stream registration; ones this agent already allocated for are left
    /// to settle via the operator. Already-paid counterparty fees are not
    /// refunded — still prefer flipping during a quiet V1 window.
    /// Env override: RFQ_V2_ONLY (LP-gated).
    pub rfq_v2_only: bool,

    // Settlement throttle
    pub max_active_settlements: usize,

    /// Max active (pending) settlements per counterparty. New proposals from a
    /// counterparty already at the cap are refused (preconfirmation reject) —
    /// prevents a broken/spamming counterparty from piling up one-sided
    /// settlements that would otherwise sit until the server timeout.
    pub max_pending_per_counterparty: usize,

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

    /// RFQ V2 (AtomicDVP) secp256k1 quote-signing keypair. Loaded in `assemble`
    /// iff `liquidity_provider.rfq_v2.enabled`, from the
    /// ATOMIC_QUOTE_PRIVATE_KEY env var (raw 32-byte scalar hex) ONLY — no
    /// keyfiles, so the runtime and every CLI path share one key source.
    pub atomic_quote_key: Option<AtomicQuoteKey>,
}

/// RFQ V2 quote-signing key: public SPKI hex plus the sealed private scalar,
/// with a redacting `Debug`.
#[derive(Clone)]
pub struct AtomicQuoteKey {
    /// X.509 SPKI DER with uncompressed point, lowercase hex
    pub pub_spki_hex: String,
    scalar: Secret<32>,
}

impl AtomicQuoteKey {
    /// Build from a raw 32-byte scalar (64 hex chars); derives the public key.
    pub fn from_scalar_hex(scalar_hex: &str) -> Result<Self> {
        let mut kf = atomic_quote::keyfile_from_scalar(scalar_hex.trim())?;
        let decoded = hex::decode(&kf.priv_scalar_hex).map(Zeroizing::new);
        kf.priv_scalar_hex.zeroize();
        let decoded = decoded?;
        let mut bytes: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("quote scalar must be 32 bytes"))?;
        Ok(Self {
            pub_spki_hex: kf.pub_spki_hex,
            scalar: Secret::seal(&mut bytes),
        })
    }

    /// Sealed scalar, exposed briefly for signing.
    pub fn scalar(&self) -> crate::secret::Exposed<32> {
        self.scalar.expose()
    }

    /// Lowercase hex of the scalar; the returned string is zeroed on drop.
    pub fn scalar_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(self.scalar.expose().as_slice()))
    }
}

impl std::fmt::Debug for AtomicQuoteKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtomicQuoteKey")
            .field("pub_spki_hex", &self.pub_spki_hex)
            .finish_non_exhaustive()
    }
}

impl BaseConfig {
    /// Minimal config for unit tests. Only the fields a test actually reads
    /// matter; everything else is a zero/empty placeholder. Centralized here so
    /// adding a `BaseConfig` field breaks one place, not every executor test.
    /// `pub` (not `cfg(test)`) so dependent crates' test modules (cloud-agent's
    /// rfq_v2 tests) can build a state harness; hidden from docs — never use
    /// outside tests.
    #[doc(hidden)]
    pub fn test_minimal() -> Self {
        Self {
            orderbook_grpc_url: String::new(),
            synchronizer_id: String::new(),
            party_id: "test-party".to_string(),
            private_key: Secret::seal(&mut [0u8; 32]),
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
            instrument_wire_ids: HashMap::new(),
            auto_settle: false,
            poll_interval_secs: 0,
            role: "agent".to_string(),
            token_ttl_secs: 60,
            connection_timeout_secs: 10,
            request_timeout_secs: 10,
            canton_op_timeout_secs: 60,
            markets: Vec::new(),
            venue_overrides: Vec::new(),
            node_name: String::new(),
            venue_branch: None,
            ledger_service_public_key: [0u8; 32],
            liquidity_provider: None,
            rfq_v2_only: false,
            max_active_settlements: 1000,
            max_pending_per_counterparty: 1000,
            settle_before_secs: 1800,
            allocate_before_secs: 900,
            liquidity_margin: 1.1,
            flow_ema_window_hours: 4.0,
            depletion_max_hours: 12.0,
            depletion_min_hours: 1.0,
            min_prepaid_traffic_balance_cc: None,
            prepaid_traffic_topup_cc: None,
            atomic_quote_key: None,
        }
    }
}

/// CLI-supplied values that take precedence over the corresponding env vars.
#[derive(Default)]
pub struct ConfigOverrides {
    /// Overrides `PARTY_AGENT`.
    pub party: Option<String>,
    /// Overrides `PARTY_AGENT_PRIVATE_KEY` (base58).
    pub private_key: Option<Zeroizing<String>>,
    /// Overrides `ATOMIC_QUOTE_PRIVATE_KEY` (hex scalar).
    pub quote_private_key: Option<Zeroizing<String>>,
}

impl BaseConfig {
    /// Strict loader — fails if `agent.toml` is missing or unparseable.
    /// Use from commands that actually consume market/LP settings (i.e. `agent`).
    pub fn load<P: AsRef<Path>>(agent_toml_path: P) -> Result<Self> {
        Self::load_with(agent_toml_path, ConfigOverrides::default())
    }

    /// [`BaseConfig::load`] with CLI-supplied overrides for the env-sourced
    /// identity fields.
    pub fn load_with<P: AsRef<Path>>(
        agent_toml_path: P,
        overrides: ConfigOverrides,
    ) -> Result<Self> {
        let agent_toml_str = fs::read_to_string(agent_toml_path.as_ref())
            .with_context(|| format!("Failed to read {}", agent_toml_path.as_ref().display()))?;
        let agent: AgentToml = toml::from_str(&agent_toml_str)
            .with_context(|| format!("Failed to parse {}", agent_toml_path.as_ref().display()))?;
        Self::assemble(agent, overrides)
    }

    /// Lenient loader — if `agent.toml` is missing, use serde defaults
    /// (empty markets, no LP, default timeouts). Still fails if the file
    /// exists but is malformed, and still requires the mandatory env vars.
    /// Use from commands that don't need market/LP config (faucet, transfer, etc.).
    pub fn load_or_defaults<P: AsRef<Path>>(agent_toml_path: P) -> Result<Self> {
        Self::load_or_defaults_with(agent_toml_path, ConfigOverrides::default())
    }

    /// [`BaseConfig::load_or_defaults`] with CLI-supplied overrides for the
    /// env-sourced identity fields.
    pub fn load_or_defaults_with<P: AsRef<Path>>(
        agent_toml_path: P,
        overrides: ConfigOverrides,
    ) -> Result<Self> {
        let agent: AgentToml = if agent_toml_path.as_ref().exists() {
            let s = fs::read_to_string(agent_toml_path.as_ref()).with_context(|| {
                format!("Failed to read {}", agent_toml_path.as_ref().display())
            })?;
            toml::from_str(&s).with_context(|| {
                format!("Failed to parse {}", agent_toml_path.as_ref().display())
            })?
        } else {
            // Empty string round-trips to AgentToml with all serde defaults.
            toml::from_str("").expect("AgentToml serde defaults must parse")
        };
        Self::assemble(agent, overrides)
    }

    /// Programmatic constructor for embedding the agent as a LIBRARY — e.g. a
    /// test harness running MANY agents in one process, where the env-driven
    /// `load`/`load_or_defaults` path cannot work (process env is global, so
    /// one agent's `PARTY_AGENT`/keys would clobber another's).
    ///
    /// Fills every field not covered by the arguments with the same defaults
    /// the env path uses (serde defaults + `assemble` literals). All fields
    /// are `pub`: callers then set `markets`, `liquidity_provider`,
    /// registries (via [`BaseConfig::populate_instruments_from_rpc`]) and the
    /// quote key (via [`BaseConfig::set_atomic_quote_scalar`]) directly, and
    /// SHOULD call [`BaseConfig::validate_v2`] before running RFQ V2 — the
    /// env path's validation lives in `assemble` and does not run here.
    #[allow(clippy::too_many_arguments)]
    pub fn for_party(
        party_id: &str,
        private_key_base58: &str,
        orderbook_grpc_url: &str,
        synchronizer_id: &str,
        settlement_operator: &str,
        dso_party: &str,
        node_name: &str,
        ledger_service_public_key_base58: &str,
    ) -> Result<Self> {
        let mut private_key_bytes = decode_private_key(private_key_base58)?;
        let public_key_hex = get_public_key_hex(&private_key_bytes);
        let private_key = Secret::seal(&mut private_key_bytes);
        let ledger_service_public_key = decode_public_key(ledger_service_public_key_base58)?;
        Ok(Self {
            orderbook_grpc_url: orderbook_grpc_url.to_string(),
            synchronizer_id: synchronizer_id.to_string(),
            party_id: party_id.to_string(),
            private_key,
            public_key_hex,
            settlement_operator: settlement_operator.to_string(),
            fee_reserve_cc: 5.0,
            merge_threshold: None,
            merge_max_amulets: 100,
            merge_poll_interval_sec: 600,
            settlement_thread_count: 25,
            dso_party: dso_party.to_string(),
            onboarded_registries: Vec::new(),
            cc_token_id: None,
            instrument_registries: HashMap::new(),
            instrument_wire_ids: HashMap::new(),
            auto_settle: default_auto_settle(),
            poll_interval_secs: default_poll_interval_secs(),
            role: default_role(),
            token_ttl_secs: default_token_ttl_secs(),
            connection_timeout_secs: default_connection_timeout_secs(),
            request_timeout_secs: default_request_timeout_secs(),
            canton_op_timeout_secs: default_canton_op_timeout_secs(),
            markets: Vec::new(),
            venue_overrides: Vec::new(),
            node_name: node_name.to_string(),
            venue_branch: crate::auth::venue_branch_from_env("VENUE_BRANCH"),
            ledger_service_public_key,
            liquidity_provider: None,
            rfq_v2_only: false,
            max_active_settlements: 10,
            max_pending_per_counterparty: 10,
            settle_before_secs: default_rfq_settle_before_secs() as u64,
            allocate_before_secs: default_rfq_allocate_before_secs() as u64,
            liquidity_margin: 1.1,
            flow_ema_window_hours: 4.0,
            depletion_max_hours: 12.0,
            depletion_min_hours: 1.0,
            min_prepaid_traffic_balance_cc: None,
            prepaid_traffic_topup_cc: None,
            atomic_quote_key: None,
        })
    }

    /// Re-derive the top-level `settle_before_secs` / `allocate_before_secs`
    /// from the per-market `[markets.rfq]` windows — the same maxima
    /// `assemble` computes on the env path. [`BaseConfig::for_party`] embedders
    /// MUST call this after setting `markets`, or the v1 settlement-expiry
    /// logic will keep judging settlements against the serde defaults
    /// (1800/900) while quotes advertise the per-market windows.
    pub fn recompute_deadline_windows(&mut self) {
        self.settle_before_secs = self
            .markets
            .iter()
            .filter_map(|m| m.rfq.as_ref())
            .map(|r| r.settle_before_secs as u64)
            .max()
            .unwrap_or(default_rfq_settle_before_secs() as u64);
        self.allocate_before_secs = self
            .markets
            .iter()
            .filter_map(|m| m.rfq.as_ref())
            .map(|r| r.allocate_before_secs as u64)
            .max()
            .unwrap_or(default_rfq_allocate_before_secs() as u64);
    }

    /// Set the RFQ V2 secp256k1 quote-signing key from a raw 32-byte scalar
    /// (64-char hex) — the same format `ATOMIC_QUOTE_PRIVATE_KEY` carries on
    /// the env path (`assemble`). For [`BaseConfig::for_party`] embedders that
    /// load per-agent keys from their own store instead of process env.
    pub fn set_atomic_quote_scalar(&mut self, scalar_hex: &str) -> Result<()> {
        self.atomic_quote_key = Some(
            AtomicQuoteKey::from_scalar_hex(scalar_hex)
                .context("quote key is not a valid secp256k1 scalar")?,
        );
        Ok(())
    }

    /// The RFQ V2 / market-window validation `assemble` runs on the env path,
    /// for configs built programmatically via [`BaseConfig::for_party`]. Call
    /// after setting `markets` / `liquidity_provider` / the quote key.
    pub fn validate_v2(&self) -> Result<()> {
        for market in &self.markets {
            if let Some(rfq) = &market.rfq {
                if rfq.allocate_before_secs == 0
                    || rfq.allocate_before_secs >= rfq.settle_before_secs
                {
                    return Err(anyhow!(
                        "Market {}: invalid [markets.rfq] deadline windows: \
                         allocate_before_secs={} settle_before_secs={} \
                         (need 0 < allocate_before_secs < settle_before_secs)",
                        market.market_id,
                        rfq.allocate_before_secs,
                        rfq.settle_before_secs
                    ));
                }
                if rfq.settle_before_secs as u64 > self.settle_before_secs
                    || rfq.allocate_before_secs as u64 > self.allocate_before_secs
                {
                    return Err(anyhow!(
                        "Market {}: [markets.rfq] windows exceed the top-level \
                         settle/allocate_before_secs — call \
                         BaseConfig::recompute_deadline_windows() after setting markets",
                        market.market_id
                    ));
                }
            }
        }

        let rfq_v2_enabled = self
            .liquidity_provider
            .as_ref()
            .and_then(|lp| lp.rfq_v2.as_ref())
            .map(|v2| v2.enabled)
            .unwrap_or(false);

        for market in &self.markets {
            if let Some(v2m) = market.rfq.as_ref().and_then(|r| r.v2.as_ref()) {
                if !(1..=100).contains(&v2m.max_input_holdings) {
                    return Err(anyhow!(
                        "Market {}: [markets.rfq.v2] max_input_holdings={} must be 1..=100 \
                         (relay protocol hard bound)",
                        market.market_id,
                        v2m.max_input_holdings
                    ));
                }
                if v2m.enabled && !rfq_v2_enabled {
                    return Err(anyhow!(
                        "Market {} has [markets.rfq.v2] enabled but \
                         [liquidity_provider.rfq_v2].enabled is not set",
                        market.market_id
                    ));
                }
            }
        }

        if let Some(v2) = self
            .liquidity_provider
            .as_ref()
            .and_then(|lp| lp.rfq_v2.as_ref())
        {
            if v2.ticket_batch_size == 0 {
                return Err(anyhow!(
                    "[liquidity_provider.rfq_v2] ticket_batch_size must be > 0"
                ));
            }
            if v2.atomic_quote_valid_secs == 0 {
                return Err(anyhow!(
                    "[liquidity_provider.rfq_v2] atomic_quote_valid_secs must be > 0"
                ));
            }
        }

        if rfq_v2_enabled && self.atomic_quote_key.is_none() {
            return Err(anyhow!(
                "RFQ V2 is enabled but no quote key is set — call \
                 BaseConfig::set_atomic_quote_scalar (or set ATOMIC_QUOTE_PRIVATE_KEY on the env path)"
            ));
        }

        if self.rfq_v2_only {
            if self.liquidity_provider.is_none() {
                return Err(anyhow!(
                    "rfq_v2_only = true requires a [liquidity_provider] section"
                ));
            }
            if !rfq_v2_enabled {
                return Err(anyhow!(
                    "rfq_v2_only = true requires [liquidity_provider.rfq_v2].enabled = true \
                     (or RFQ_V2_ENABLED=true on the env path)"
                ));
            }
            // Same predicate as setup_rfq_v2's runtime market filter:
            // market.enabled && rfq.enabled && rfq.v2.enabled.
            let any_v2_market = self.markets.iter().any(|m| {
                m.enabled
                    && m.rfq
                        .as_ref()
                        .is_some_and(|r| r.enabled && r.v2.as_ref().is_some_and(|v| v.enabled))
            });
            if !any_v2_market {
                return Err(anyhow!(
                    "rfq_v2_only = true requires at least one enabled market with \
                     [markets.rfq].enabled and [markets.rfq.v2].enabled = true — \
                     otherwise the agent would quote nothing"
                ));
            }
        }
        Ok(())
    }

    /// Assemble the full BaseConfig from an already-parsed AgentToml +
    /// env vars. Instrument/registry info is populated later by
    /// [`BaseConfig::populate_instruments_from_rpc`]. Shared between
    /// strict/lenient loaders.
    fn assemble(mut agent: AgentToml, overrides: ConfigOverrides) -> Result<Self> {
        // Instrument registry placeholders — filled by populate_instruments_from_rpc
        // after the cloud-agent fetches them over gRPC at startup.
        let onboarded_registries: Vec<String> = Vec::new();
        let cc_token_id: Option<String> = None;
        let instrument_registries: HashMap<String, String> = HashMap::new();
        let instrument_wire_ids: HashMap<String, String> = HashMap::new();

        // Read env vars (CLI overrides win where provided)
        let dso_party = std::env::var("DSO").map_err(|_| anyhow!("DSO env var is required"))?;

        let party_id = match overrides.party.filter(|s| !s.trim().is_empty()) {
            Some(p) => p,
            None => std::env::var("PARTY_AGENT")
                .map_err(|_| anyhow!("PARTY_AGENT env var (or --party) is required"))?,
        };

        let private_key_b58: Zeroizing<String> = match overrides
            .private_key
            .filter(|s| !s.trim().is_empty())
        {
            Some(k) => k,
            None => Zeroizing::new(std::env::var("PARTY_AGENT_PRIVATE_KEY").map_err(|_| {
                anyhow!("PARTY_AGENT_PRIVATE_KEY env var (or --private-key) is required")
            })?),
        };

        let mut private_key_bytes = decode_private_key(&private_key_b58)?;
        let public_key_hex = get_public_key_hex(&private_key_bytes);
        let private_key = Secret::seal(&mut private_key_bytes);
        drop(private_key_b58);

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

        let merge_threshold = std::env::var("MERGE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok());
        let merge_max_amulets = std::env::var("MERGE_MAX_AMULETS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        let merge_poll_interval_sec = std::env::var("MERGE_POLL_INTERVAL_SEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);

        let settlement_thread_count = std::env::var("SETTLEMENT_THREAD_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(25);

        let max_active_settlements = std::env::var("AGENT_MAX_SETTLEMENTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        let max_pending_per_counterparty = std::env::var("AGENT_MAX_PENDING_PER_COUNTERPARTY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        let node_name =
            std::env::var("NODE_NAME").map_err(|_| anyhow!("NODE_NAME env var is required"))?;

        let ledger_service_public_key_b58 = std::env::var("LEDGER_SERVICE_PUBLIC_KEY")
            .map_err(|_| anyhow!("LEDGER_SERVICE_PUBLIC_KEY env var is required"))?;
        let ledger_service_public_key = decode_public_key(&ledger_service_public_key_b58)?;

        // Clamp any out-of-range [markets.rfq.pool_impact] knobs — warn and
        // run (the section is an additive protection, not a precondition).
        for market in &mut agent.markets {
            if let Some(rfq) = &mut market.rfq {
                if let Some(pi) = &mut rfq.pool_impact {
                    pi.sanitize(&market.market_id);
                }
            }
        }

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
                        market.market_id,
                        rfq.allocate_before_secs,
                        rfq.settle_before_secs
                    ));
                }
            }
        }

        let settle_before_secs = agent
            .markets
            .iter()
            .filter_map(|m| m.rfq.as_ref())
            .map(|r| r.settle_before_secs as u64)
            .max()
            .unwrap_or(default_rfq_settle_before_secs() as u64);

        let allocate_before_secs = agent
            .markets
            .iter()
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
        let (min_prepaid_traffic_balance_cc, prepaid_traffic_topup_cc) = match (
            min_prepaid,
            topup_prepaid,
        ) {
            (Some(min_str), Some(topup_str)) => {
                let min: rust_decimal::Decimal = min_str.parse().with_context(|| {
                    format!(
                        "MIN_PREPAID_TRAFFIC_BALANCE_CC must be a decimal, got '{}'",
                        min_str
                    )
                })?;
                let topup: rust_decimal::Decimal = topup_str.parse().with_context(|| {
                    format!(
                        "PREPAID_TRAFFIC_TOPUP_CC must be a decimal, got '{}'",
                        topup_str
                    )
                })?;
                if topup <= rust_decimal::Decimal::ZERO {
                    return Err(anyhow!(
                        "PREPAID_TRAFFIC_TOPUP_CC must be > 0, got {}",
                        topup
                    ));
                }
                (Some(min), Some(topup))
            }
            (None, None) => (None, None),
            (Some(_), None) => {
                return Err(anyhow!(
                    "MIN_PREPAID_TRAFFIC_BALANCE_CC is set but PREPAID_TRAFFIC_TOPUP_CC is not — both required for auto-topup"
                ));
            }
            (None, Some(_)) => {
                return Err(anyhow!(
                    "PREPAID_TRAFFIC_TOPUP_CC is set but MIN_PREPAID_TRAFFIC_BALANCE_CC is not — both required for auto-topup"
                ));
            }
        };

        // --- RFQ V2 env overrides (MERGE_* idiom) ---
        if let Some(ref mut lp) = agent.liquidity_provider {
            let rfq_v2_env: Option<bool> = std::env::var("RFQ_V2_ENABLED")
                .ok()
                .and_then(|v| v.parse().ok());
            let ticket_threshold_env: Option<f64> = std::env::var("TICKET_THRESHOLD_USD")
                .ok()
                .and_then(|v| v.parse().ok());
            let ticket_batch_env: Option<usize> = std::env::var("TICKET_BATCH_SIZE")
                .ok()
                .and_then(|v| v.parse().ok());
            if rfq_v2_env.is_some() || ticket_threshold_env.is_some() || ticket_batch_env.is_some()
            {
                let v2 = lp.rfq_v2.get_or_insert_with(RfqV2Config::default);
                if let Some(enabled) = rfq_v2_env {
                    v2.enabled = enabled;
                }
                if let Some(threshold) = ticket_threshold_env {
                    v2.ticket_threshold_usd = Some(threshold);
                }
                if let Some(batch) = ticket_batch_env {
                    v2.ticket_batch_size = batch;
                }
            }
        }

        // RFQ_V2_ONLY env override — LP-gated like RFQ_V2_ENABLED, so an
        // exported env var cannot fail unrelated utility commands running on
        // the load_or_defaults path in a directory without a
        // [liquidity_provider] section. A toml-sourced `rfq_v2_only = true`
        // without an LP section is still a hard error below.
        if agent.liquidity_provider.is_some() {
            if let Some(only) = std::env::var("RFQ_V2_ONLY")
                .ok()
                .and_then(|v| v.parse::<bool>().ok())
            {
                agent.rfq_v2_only = only;
            }
        }

        // --- RFQ V2 validation + quote-key loading ---
        let rfq_v2_enabled = agent
            .liquidity_provider
            .as_ref()
            .and_then(|lp| lp.rfq_v2.as_ref())
            .map(|v2| v2.enabled)
            .unwrap_or(false);

        let global_ladders = agent
            .liquidity_provider
            .as_ref()
            .and_then(|lp| lp.rfq_v2.as_ref())
            .map(|v2| !v2.denominations.is_empty())
            .unwrap_or(false);
        for market in &agent.markets {
            if let Some(v2m) = market.rfq.as_ref().and_then(|r| r.v2.as_ref()) {
                if !(1..=100).contains(&v2m.max_input_holdings) {
                    return Err(anyhow!(
                        "Market {}: [markets.rfq.v2] max_input_holdings={} must be 1..=100 \
                         (relay protocol hard bound)",
                        market.market_id,
                        v2m.max_input_holdings
                    ));
                }
                if global_ladders && !v2m.denominations.is_empty() {
                    tracing::warn!(
                        "Market {}: per-market [markets.rfq.v2].denominations is superseded by \
                         the global [liquidity_provider.rfq_v2.denominations] map and ignored",
                        market.market_id
                    );
                }
                if v2m.enabled && !rfq_v2_enabled {
                    return Err(anyhow!(
                        "Market {} has [markets.rfq.v2] enabled but \
                         [liquidity_provider.rfq_v2].enabled is not set — enable the LP-level \
                         switch (or RFQ_V2_ENABLED=true) or disable the market",
                        market.market_id
                    ));
                }
            }
        }

        // --- [[venue_overrides]] validation (RFQ V2 venue/branch overlays) ---
        // Invalid slugs are hard errors: a typoed venue would otherwise just
        // silently never match and the operator would ship pair-default
        // pricing believing the override was live.
        for (i, ov) in agent.venue_overrides.iter().enumerate() {
            if !crate::auth::is_valid_venue_branch(&ov.venue) {
                return Err(anyhow!(
                    "[[venue_overrides]] #{}: venue '{}' is not a valid slug \
                     (^[a-z0-9][a-z0-9-]{{1,19}}$) — it must equal the server's \
                     swap-venue name (AtomicRfqRequest.venue_name)",
                    i + 1,
                    ov.venue
                ));
            }
            if let Some(ref b) = ov.branch {
                if !crate::auth::is_valid_venue_branch(b) {
                    return Err(anyhow!(
                        "[[venue_overrides]] #{} (venue '{}'): branch '{}' is not a \
                         valid slug (^[a-z0-9][a-z0-9-]{{1,19}}$)",
                        i + 1,
                        ov.venue,
                        b
                    ));
                }
            }
            if let Some(ref markets) = ov.markets {
                if markets.is_empty() {
                    return Err(anyhow!(
                        "[[venue_overrides]] #{} (venue '{}'): markets = [] can never \
                         match — omit the key entirely to target all markets",
                        i + 1,
                        ov.venue
                    ));
                }
                for m in markets {
                    if !agent.markets.iter().any(|mc| &mc.market_id == m) {
                        tracing::warn!(
                            "[[venue_overrides]] #{} (venue '{}'): market '{}' is not in \
                             [[markets]] — that scope entry can never match",
                            i + 1,
                            ov.venue,
                            m
                        );
                    }
                    // The "open" direction is unsupported: a market whose own
                    // [markets.rfq] is disabled is never subscribed on the V2
                    // stream, so an override claiming to enable it would be a
                    // policy that silently does not exist.
                    if ov.rfq.enabled == Some(true) {
                        let pair_enabled = agent
                            .markets
                            .iter()
                            .find(|mc| &mc.market_id == m)
                            .and_then(|mc| mc.rfq.as_ref())
                            .map(|r| r.enabled)
                            .unwrap_or(false);
                        if !pair_enabled {
                            return Err(anyhow!(
                                "[[venue_overrides]] #{} (venue '{}'): enabled = true on \
                                 market '{}' whose [markets.rfq] is disabled or absent — \
                                 overrides cannot OPEN a pair-disabled market (the V2 \
                                 stream never subscribes it); enable the pair and close \
                                 the other venues instead",
                                i + 1,
                                ov.venue,
                                m
                            ));
                        }
                    }
                }
            }
            if ov.rfq.is_empty() {
                tracing::warn!(
                    "[[venue_overrides]] #{} (venue '{}'): empty [venue_overrides.rfq] \
                     overlay — entry has no effect",
                    i + 1,
                    ov.venue
                );
            }
            // Bounds must PARSE (a typo would otherwise silently become
            // min 0.0 / max f64::MAX at runtime), and be ordered when both set.
            let min = match &ov.rfq.min_quantity {
                Some(s) => Some(s.parse::<f64>().map_err(|_| {
                    anyhow!(
                        "[[venue_overrides]] #{} (venue '{}'): min_quantity '{}' is not \
                         a number — the runtime fallback would silently disable the floor",
                        i + 1,
                        ov.venue,
                        s
                    )
                })?),
                None => None,
            };
            let max = match &ov.rfq.max_quantity {
                Some(s) => Some(s.parse::<f64>().map_err(|_| {
                    anyhow!(
                        "[[venue_overrides]] #{} (venue '{}'): max_quantity '{}' is not \
                         a number — the runtime fallback would silently remove the cap",
                        i + 1,
                        ov.venue,
                        s
                    )
                })?),
                None => None,
            };
            if let (Some(min), Some(max)) = (min, max) {
                if min > max {
                    return Err(anyhow!(
                        "[[venue_overrides]] #{} (venue '{}'): min_quantity {} > \
                         max_quantity {}",
                        i + 1,
                        ov.venue,
                        min,
                        max
                    ));
                }
            }
        }

        if let Some(v2) = agent
            .liquidity_provider
            .as_ref()
            .and_then(|lp| lp.rfq_v2.as_ref())
        {
            if v2.ticket_batch_size == 0 {
                return Err(anyhow!(
                    "[liquidity_provider.rfq_v2] ticket_batch_size must be > 0"
                ));
            }
            if v2.atomic_quote_valid_secs == 0 {
                return Err(anyhow!(
                    "[liquidity_provider.rfq_v2] atomic_quote_valid_secs must be > 0"
                ));
            }
        }

        // rfq_v2_only sanity: with V1 and orders disabled, a config without a
        // working V2 stack would quote NOTHING — fail loud at startup instead.
        // Runs after the env overrides above so RFQ_V2_ENABLED/RFQ_V2_ONLY count.
        if agent.rfq_v2_only {
            if agent.liquidity_provider.is_none() {
                return Err(anyhow!(
                    "rfq_v2_only = true requires a [liquidity_provider] section"
                ));
            }
            if !rfq_v2_enabled {
                return Err(anyhow!(
                    "rfq_v2_only = true requires [liquidity_provider.rfq_v2].enabled = true \
                     (or RFQ_V2_ENABLED=true)"
                ));
            }
            // Same predicate as setup_rfq_v2's runtime market filter:
            // market.enabled && rfq.enabled && rfq.v2.enabled.
            let any_v2_market = agent.markets.iter().any(|m| {
                m.enabled
                    && m.rfq
                        .as_ref()
                        .is_some_and(|r| r.enabled && r.v2.as_ref().is_some_and(|v| v.enabled))
            });
            if !any_v2_market {
                return Err(anyhow!(
                    "rfq_v2_only = true requires at least one enabled market with \
                     [markets.rfq].enabled and [markets.rfq.v2].enabled = true — \
                     otherwise the agent would quote nothing"
                ));
            }
        }

        // Quote key: required iff RFQ V2 is enabled — `--quote-private-key`,
        // else ATOMIC_QUOTE_PRIVATE_KEY (raw 32-byte scalar hex).
        let atomic_quote_key = if rfq_v2_enabled {
            let (scalar, source): (Zeroizing<String>, &str) = match overrides
                .quote_private_key
                .filter(|s| !s.trim().is_empty())
            {
                Some(s) => (s, "--quote-private-key"),
                None => (
                    Zeroizing::new(
                        std::env::var("ATOMIC_QUOTE_PRIVATE_KEY")
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                            .context(
                                "RFQ V2 is enabled but ATOMIC_QUOTE_PRIVATE_KEY is not set — \
                                 run `atomic keygen` and add the printed line to .env, \
                                 or pass --quote-private-key",
                            )?,
                    ),
                    "ATOMIC_QUOTE_PRIVATE_KEY",
                ),
            };
            Some(
                AtomicQuoteKey::from_scalar_hex(&scalar)
                    .with_context(|| format!("{source} is not a valid secp256k1 scalar"))?,
            )
        } else {
            None
        };

        Ok(BaseConfig {
            orderbook_grpc_url,
            synchronizer_id,
            party_id,
            private_key,
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
            instrument_wire_ids,
            auto_settle: agent.auto_settle,
            poll_interval_secs: agent.poll_interval_secs,
            role: agent.role,
            token_ttl_secs: agent.token_ttl_secs,
            connection_timeout_secs: agent.connection_timeout_secs,
            request_timeout_secs: agent.request_timeout_secs,
            canton_op_timeout_secs: agent.canton_op_timeout_secs,
            markets: agent.markets,
            venue_overrides: agent.venue_overrides,
            node_name,
            // Production cloud-agent deployments set VENUE_BRANCH=agent (VA13).
            venue_branch: crate::auth::venue_branch_from_env("VENUE_BRANCH"),
            ledger_service_public_key,
            liquidity_provider: agent.liquidity_provider,
            rfq_v2_only: agent.rfq_v2_only,
            max_active_settlements,
            max_pending_per_counterparty,
            settle_before_secs,
            allocate_before_secs,
            liquidity_margin,
            flow_ema_window_hours,
            depletion_max_hours,
            depletion_min_hours,
            min_prepaid_traffic_balance_cc,
            prepaid_traffic_topup_cc,
            atomic_quote_key,
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
        let mut instrument_wire_ids: HashMap<String, String> = HashMap::new();
        let mut cc_token_id: Option<String> = None;
        let mut onboarded_registries: HashSet<String> = HashSet::new();

        for inst in instruments {
            let registry = inst.registry.clone().unwrap_or_default();
            if !registry.is_empty() {
                instrument_registries.insert(inst.instrument_id.clone(), registry.clone());
                onboarded_registries.insert(registry);
            }
            // The ON-CHAIN wire id is `symbol` (== instrument_id for legacy
            // tokens; an opaque id, e.g. a UUID, for issuer-minted ones). Fall back to the
            // id when an older RPC serves an empty symbol.
            let wire_id = if inst.symbol.is_empty() {
                inst.instrument_id.clone()
            } else {
                inst.symbol.clone()
            };
            instrument_wire_ids.insert(inst.instrument_id.clone(), wire_id);
            if inst.instrument_type == "token" && cc_token_id.is_none() {
                cc_token_id = Some(inst.instrument_id.clone());
            }
        }

        self.cc_token_id = cc_token_id;
        self.instrument_registries = instrument_registries;
        self.instrument_wire_ids = instrument_wire_ids;
        self.onboarded_registries = onboarded_registries.into_iter().collect();
        tracing::info!(
            "Instrument registry populated from RPC: {} instruments, {} registries, cc_token_id={:?}",
            self.instrument_registries.len(),
            self.onboarded_registries.len(),
            self.cc_token_id,
        );
    }

    /// ON-CHAIN wire id → orderbook-internal instrument id.
    ///
    /// `None` when unknown or when the ids coincide — legacy tokens never need
    /// translation, so callers can use this as a "does this need renaming"
    /// test. Linear scan over a handful of instruments.
    pub fn internal_id_for_wire(&self, wire_id: &str) -> Option<String> {
        self.instrument_wire_ids
            .iter()
            .find(|(internal, wire)| wire.as_str() == wire_id && internal.as_str() != wire_id)
            .map(|(internal, _)| internal.clone())
    }

    /// Resolve an orderbook instrument ID to (on_chain_id, registry_party).
    ///
    /// CC maps to on-chain "Amulet"; other instruments map to their WIRE id
    /// (`instruments.symbol` — identical to the id for legacy tokens, an
    /// opaque id such as a UUID for issuer-minted ones).
    /// Registry comes from instruments fetched via `populate_instruments_from_rpc`.
    /// Returns empty strings if instrument not found (verification skipped).
    pub fn resolve_instrument(&self, instrument_id: &str) -> (String, String) {
        let on_chain_id = if Some(instrument_id) == self.cc_token_id.as_deref() {
            "Amulet".to_string()
        } else {
            self.instrument_wire_ids
                .get(instrument_id)
                .cloned()
                .unwrap_or_else(|| instrument_id.to_string())
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
    let key_bytes = Zeroizing::new(
        bs58::decode(base58_key.trim())
            .into_vec()
            .context("Invalid base58 private key")?,
    );

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

/// Check that `base58_key` decodes to a usable private key.
pub fn validate_private_key(base58_key: &str) -> Result<()> {
    let mut bytes = decode_private_key(base58_key)?;
    bytes.zeroize();
    Ok(())
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
            tracing::info!("Loaded {} registries from {}", config.registry.len(), path);
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
    /// Disable the sequencer-load spread multiplier (3x overload / 2x LOW
    /// forecast) for this market: quotes always use the raw configured spread
    /// regardless of load.
    #[serde(default)]
    pub disable_overload_spread_widening: bool,
    /// Disable the one-sided depletion spread widening for this market: the
    /// depletion coefficient of the LP-pays token is ignored when pricing.
    #[serde(default)]
    pub disable_depletion_spread_widening: bool,
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
    /// Size-aware pricing — `[markets.rfq.pool_impact]`. Applies to RFQ V2 and
    /// this market's offer grid. Absent = no adjustment. Not per-venue.
    #[serde(default)]
    pub pool_impact: Option<PoolImpactConfig>,
    /// RFQ V2 (AtomicDVP) per-market configuration — TOML `[markets.rfq.v2]`.
    #[serde(default)]
    pub v2: Option<RfqV2MarketConfig>,
}

/// Size-aware pricing knobs (`[markets.rfq.pool_impact]`). The math lives in
/// [`crate::pool_impact`], which clamps every field defensively.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolImpactConfig {
    /// false (default) = SHADOW MODE: compute + log the would-be impact but
    /// do not apply it to any price or grid rung. true = enforce.
    #[serde(default)]
    pub enabled: bool,
    /// Hard cap on the applied impact, percent of mid.
    #[serde(default = "default_max_impact_percent")]
    pub max_impact_percent: f64,
    /// Cap on the pool fraction the impact curve is evaluated at (u clamp).
    #[serde(default = "default_max_pool_fraction")]
    pub max_pool_fraction: f64,
    /// Scales the marginal cost before the cap. >1 compensates for flow this
    /// agent cannot observe.
    #[serde(default = "default_impact_multiplier")]
    pub impact_multiplier: f64,
    /// Net position (base units) exempt from any charge, per counterparty.
    #[serde(default)]
    pub free_zone_base: f64,
    /// Trailing-net decay window, hours. The tracker is per-token, so the
    /// effective window is the max across markets sharing that token.
    #[serde(default = "default_impact_window_hours")]
    pub window_hours: f64,
    /// Confirm-time re-check tolerance, percent: reject when the held price is
    /// taker-favourable versus the current fair price by more than this.
    #[serde(default = "default_confirm_tolerance_percent")]
    pub confirm_tolerance_percent: f64,
    /// Grid shaping: desk-net free zone for offer-rung shaping; falls back to
    /// `free_zone_base` when absent.
    #[serde(default)]
    pub grid_free_zone_base: Option<f64>,
    /// Grid shaping: desk-net span (base units) over which offer rungs scale to
    /// zero past the grid free zone. Absent = derived from the size reference.
    #[serde(default)]
    pub grid_offer_scale_base: Option<f64>,
    /// Grid: drop a shaped offer rung below this many base units — the server
    /// rejects sub-minimum orders. Absent = `DEFAULT_MIN_RUNG_FRACTION`.
    #[serde(default)]
    pub grid_min_rung_base: Option<f64>,
}

fn default_max_impact_percent() -> f64 {
    50.0
}
fn default_max_pool_fraction() -> f64 {
    0.9
}
fn default_impact_multiplier() -> f64 {
    1.0
}
fn default_impact_window_hours() -> f64 {
    24.0
}
fn default_confirm_tolerance_percent() -> f64 {
    0.25
}

impl Default for PoolImpactConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_impact_percent: default_max_impact_percent(),
            max_pool_fraction: default_max_pool_fraction(),
            impact_multiplier: default_impact_multiplier(),
            free_zone_base: 0.0,
            window_hours: default_impact_window_hours(),
            confirm_tolerance_percent: default_confirm_tolerance_percent(),
            grid_free_zone_base: None,
            grid_min_rung_base: None,
            grid_offer_scale_base: None,
        }
    }
}

impl PoolImpactConfig {
    /// Clamp out-of-range knobs, warning per change. Never panic over a
    /// config typo: this section is additive, not a correctness precondition.
    pub fn sanitize(&mut self, market_id: &str) {
        let clamp = |name: &str, v: &mut f64, lo: f64, hi: f64| {
            let c = if v.is_finite() { v.clamp(lo, hi) } else { lo };
            if c != *v {
                tracing::warn!(
                    "Market {market_id}: [markets.rfq.pool_impact] {name}={v} out of range — clamped to {c}"
                );
                *v = c;
            }
        };
        clamp("max_impact_percent", &mut self.max_impact_percent, 0.0, 95.0);
        clamp("max_pool_fraction", &mut self.max_pool_fraction, 0.01, 0.99);
        clamp("impact_multiplier", &mut self.impact_multiplier, 0.0, 100.0);
        clamp("free_zone_base", &mut self.free_zone_base, 0.0, f64::MAX);
        clamp("window_hours", &mut self.window_hours, 0.01, 720.0);
        clamp(
            "confirm_tolerance_percent",
            &mut self.confirm_tolerance_percent,
            0.0,
            100.0,
        );
        if let Some(v) = &mut self.grid_free_zone_base {
            clamp("grid_free_zone_base", v, 0.0, f64::MAX);
        }
        if let Some(v) = &mut self.grid_offer_scale_base {
            clamp("grid_offer_scale_base", v, 0.0, f64::MAX);
        }
    }
}

/// A venue/branch-scoped override of `[markets.rfq]` parameters — TOML
/// `[[venue_overrides]]`. RFQ V2 only: the venue arrives on the atomic
/// stream as `AtomicRfqRequest.venue_name` (agents fall back to the VA2
/// `quote_id_prefix` for servers predating that field) and the branch as
/// `AtomicRfqRequest.venue_branch`; V1 RFQs carry no venue identity and
/// always price at the pair defaults. Grid orders are the public
/// venue-agnostic book and are never affected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueOverride {
    /// Swap-venue slug this entry applies to (= `AtomicRfqRequest.venue_name`).
    pub venue: String,
    /// Restrict to one branch of the venue; absent = any branch. Branch-scoped
    /// entries only ever match once the server sends `venue_branch` (older
    /// servers omit it) — and the server forwards branches ONLY for delegated
    /// venue traffic (platform-minted venue JWTs); self-asserted non-delegated
    /// branches are never forwarded, so they can only match branch-less entries.
    #[serde(default)]
    pub branch: Option<String>,
    /// Restrict to these market_ids; absent = all markets.
    #[serde(default)]
    pub markets: Option<Vec<String>>,
    /// The sparse `[venue_overrides.rfq]` overlay.
    pub rfq: RfqOverlayConfig,
}

impl VenueOverride {
    fn matches(&self, market_id: &str, venue: &str, branch: Option<&str>) -> bool {
        if self.venue != venue {
            return false;
        }
        if let Some(ref b) = self.branch {
            if branch != Some(b.as_str()) {
                return false;
            }
        }
        if let Some(ref markets) = self.markets {
            if !markets.iter().any(|m| m == market_id) {
                return false;
            }
        }
        true
    }

    /// More constrained entries apply later (and therefore win) in
    /// [`resolve_rfq_config`].
    fn specificity(&self) -> u8 {
        self.branch.is_some() as u8 + self.markets.is_some() as u8
    }
}

/// Sparse all-`Option` mirror of [`RfqMarketConfig`]'s venue-effective
/// scalars: a set field replaces the pair value, an unset one inherits it.
/// Deliberately excluded — fields with NO effect on any venue-carrying
/// request (offering them would be dead config that deceives the operator):
/// `v2` (denomination ladders, structural), `min_notional_usd` (the floor is
/// V1-only, and V1 carries no venue), `allocate_before_secs` /
/// `settle_before_secs` (consumed only by the V1 quote message; V2 deadlines
/// come from the atomic-quote globals).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RfqOverlayConfig {
    /// `Some(false)` = do not quote this venue on the matched markets;
    /// `Some(true)` re-opens after a broader matching entry's close (e.g.
    /// venue-wide `enabled = false`, one branch re-enabled). It can NOT open
    /// a market whose own `[markets.rfq]` is disabled — the V2 stream never
    /// subscribes such markets, so no venue request ever reaches pricing
    /// (assemble() rejects overrides that attempt it).
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub min_quantity: Option<String>,
    #[serde(default)]
    pub max_quantity: Option<String>,
    #[serde(default)]
    pub bid_spread_percent: Option<f64>,
    #[serde(default)]
    pub offer_spread_percent: Option<f64>,
    #[serde(default)]
    pub disable_overload_spread_widening: Option<bool>,
    #[serde(default)]
    pub disable_depletion_spread_widening: Option<bool>,
    #[serde(default)]
    pub quote_valid_secs: Option<u32>,
}

impl RfqOverlayConfig {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.min_quantity.is_none()
            && self.max_quantity.is_none()
            && self.bid_spread_percent.is_none()
            && self.offer_spread_percent.is_none()
            && self.disable_overload_spread_widening.is_none()
            && self.disable_depletion_spread_widening.is_none()
            && self.quote_valid_secs.is_none()
    }

    fn apply(&self, cfg: &mut RfqMarketConfig) {
        if let Some(v) = self.enabled {
            cfg.enabled = v;
        }
        if let Some(ref v) = self.min_quantity {
            cfg.min_quantity = v.clone();
        }
        if let Some(ref v) = self.max_quantity {
            cfg.max_quantity = v.clone();
        }
        if let Some(v) = self.bid_spread_percent {
            cfg.bid_spread_percent = v;
        }
        if let Some(v) = self.offer_spread_percent {
            cfg.offer_spread_percent = v;
        }
        if let Some(v) = self.disable_overload_spread_widening {
            cfg.disable_overload_spread_widening = v;
        }
        if let Some(v) = self.disable_depletion_spread_widening {
            cfg.disable_depletion_spread_widening = v;
        }
        if let Some(v) = self.quote_valid_secs {
            cfg.quote_valid_secs = Some(v);
        }
    }
}

/// Resolve the effective RFQ config for one request: the pair's
/// `[markets.rfq]` plus every matching `[[venue_overrides]]` overlay.
///
/// Matching entries apply in ascending specificity (venue-wide first, then
/// branch-/market-scoped; ties in file order, so a later entry wins), each
/// `Some` field overwriting — a venue-wide entry can set spreads and a
/// branch-specific one can tweak a single field on top. `venue = None` (V1
/// requests, or a server that sent no prefix) borrows the pair config
/// untouched; a clone happens only when an overlay actually matched.
pub fn resolve_rfq_config<'a>(
    pair: &'a RfqMarketConfig,
    overrides: &[VenueOverride],
    market_id: &str,
    venue: Option<&str>,
    branch: Option<&str>,
) -> std::borrow::Cow<'a, RfqMarketConfig> {
    let Some(venue) = venue else {
        return std::borrow::Cow::Borrowed(pair);
    };
    let mut matching: Vec<&VenueOverride> = overrides
        .iter()
        .filter(|o| o.matches(market_id, venue, branch))
        .collect();
    if matching.is_empty() {
        return std::borrow::Cow::Borrowed(pair);
    }
    // Stable sort: equal specificity keeps file order, so later entries
    // apply later and win.
    matching.sort_by_key(|o| o.specificity());
    let mut cfg = pair.clone();
    for o in &matching {
        o.rfq.apply(&mut cfg);
    }
    std::borrow::Cow::Owned(cfg)
}

/// RFQ V2 per-market configuration (`[markets.rfq.v2]`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqV2MarketConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Pre-split denomination ladder, "AMOUNTxCOUNT" entries (e.g. "25x20").
    /// Empty = default single rung base_order_size x max_concurrent_rfqs.
    #[serde(default)]
    pub denominations: Vec<String>,
    /// Per-leg input-cid cap for LP disclosure selection. MUST be 1..=100
    /// (the relay enforces the protocol hard bound of 100). The headroom
    /// above the coverage picks is used to sweep dust holdings into the
    /// settle for consolidation.
    #[serde(default = "default_max_input_holdings")]
    pub max_input_holdings: usize,
}

/// Liquidity provider configuration (LP agents only)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityProviderConfig {
    pub name: String,
    #[serde(default = "default_max_concurrent_rfqs")]
    pub max_concurrent_rfqs: usize,
    #[serde(default = "default_quote_valid_secs")]
    pub default_quote_valid_secs: u32,
    /// Global minimum RFQ value in USD — RFQ V1 ONLY (the LP pays its own
    /// dvp+allocation fees on a V1 settle). V1 RFQs whose USD notional is
    /// below this are rejected (AmountTooSmall). RFQ V2 ignores it: the user
    /// pays every V2 fee — 3x below the server's `min_order_value_usd` — and
    /// the LP pays none, so the LP quotes any size (the base `min_quantity`
    /// bound still applies).
    /// 0 = disabled. Overridden per-market by `[markets.rfq].min_notional_usd`.
    #[serde(default)]
    pub min_notional_usd: f64,
    /// RFQ V2 (AtomicDVP) LP-level configuration — TOML `[liquidity_provider.rfq_v2]`.
    #[serde(default)]
    pub rfq_v2: Option<RfqV2Config>,
}

/// RFQ V2 LP-level configuration (`[liquidity_provider.rfq_v2]`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqV2Config {
    /// Master switch (default false).
    #[serde(default)]
    pub enabled: bool,
    /// Signed-quote validity window in seconds, counted from LP confirm time.
    #[serde(default = "default_atomic_quote_valid_secs")]
    pub atomic_quote_valid_secs: u64,
    /// Reservation/ticket TTL beyond the signed validity window.
    #[serde(default = "default_settle_grace_secs")]
    pub settle_grace_secs: u64,
    /// USD notional at/above which a SettlementTicket is attached to the quote.
    /// Absent = tickets are never created or used (all quotes ticketless).
    /// Rate unavailable at confirm = fail CLOSED to ticketed.
    #[serde(default, deserialize_with = "de_opt_f64_or_string")]
    pub ticket_threshold_usd: Option<f64>,
    #[serde(default = "default_ticket_batch_size")]
    pub ticket_batch_size: usize,
    #[serde(default = "default_ticket_low_water")]
    pub ticket_low_water: usize,
    #[serde(default = "default_split_poll_interval_secs")]
    pub split_poll_interval_secs: u64,
    #[serde(default = "default_updates_poll_interval_secs")]
    pub updates_poll_interval_secs: u64,
    /// Low-water hysteresis divisor for the denomination ladder: a rung only
    /// triggers a split when `have < max(1, count / divisor)`, and a triggered
    /// split refills the whole ladder to its full counts in one operation.
    /// `1` restores the old eager behavior (any deficit splits); values < 1
    /// are clamped to 1.
    #[serde(default = "default_split_low_water_divisor")]
    pub split_low_water_divisor: u32,
    /// Minimum seconds between split operations per instrument, enforced
    /// across BOTH the maintenance tick and the on-demand quote-time kicks.
    #[serde(default = "default_split_min_interval_secs")]
    pub split_min_interval_secs: u64,
    /// Fail-stop budget: hard cap on split operations per instrument per
    /// hour. Tripping it logs `error!` and refuses to split — a runaway
    /// split loop must be loud and self-stopping.
    #[serde(default = "default_split_max_ops_per_hour")]
    pub split_max_ops_per_hour: u32,
    /// GLOBAL per-instrument denomination ladders: symbol → "AMOUNTxCOUNT"
    /// entries (e.g. `CC = ["100x10", "250x4"]`). One ladder per instrument,
    /// shared by every market that pays that instrument — sized for the
    /// typical $10-20 settlement. Supersedes the per-market
    /// `[markets.rfq.v2].denominations` (which is warn-ignored when this map
    /// is non-empty). The smallest rung doubles as the dust threshold:
    /// holdings below it fund settles dust-first (the fewest that fit, no
    /// rung broken), else are swept as extra inputs into rung-funded settles.
    #[serde(default)]
    pub denominations: std::collections::HashMap<String, Vec<String>>,
}

impl RfqV2Config {
    /// How long a confirmed quote's count may stand before it is a leak.
    /// DERIVED from the quote lifetime — a shorter backstop would reverse it.
    pub fn stale_pending_after(&self) -> std::time::Duration {
        const STALE_PENDING_SLACK_SECS: u64 = 600;
        const STALE_PENDING_FLOOR_SECS: u64 = 900;
        let lifetime = self
            .atomic_quote_valid_secs
            .saturating_add(self.settle_grace_secs)
            .saturating_add(STALE_PENDING_SLACK_SECS);
        std::time::Duration::from_secs(lifetime.max(STALE_PENDING_FLOOR_SECS))
    }
}

impl Default for RfqV2Config {
    fn default() -> Self {
        Self {
            enabled: false,
            atomic_quote_valid_secs: default_atomic_quote_valid_secs(),
            settle_grace_secs: default_settle_grace_secs(),
            ticket_threshold_usd: None,
            ticket_batch_size: default_ticket_batch_size(),
            ticket_low_water: default_ticket_low_water(),
            split_poll_interval_secs: default_split_poll_interval_secs(),
            updates_poll_interval_secs: default_updates_poll_interval_secs(),
            split_low_water_divisor: default_split_low_water_divisor(),
            split_min_interval_secs: default_split_min_interval_secs(),
            split_max_ops_per_hour: default_split_max_ops_per_hour(),
            denominations: Default::default(),
        }
    }
}

/// Accept `ticket_threshold_usd = 1000`, `= 1000.0`, or `= "1000"` (the design
/// doc examples use a quoted string).
fn de_opt_f64_or_string<'de, D>(deserializer: D) -> std::result::Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(f64),
        Str(String),
    }
    match Option::<NumOrStr>::deserialize(deserializer)? {
        None => Ok(None),
        Some(NumOrStr::Num(n)) => Ok(Some(n)),
        Some(NumOrStr::Str(s)) => s
            .parse::<f64>()
            .map(Some)
            .map_err(|e| serde::de::Error::custom(format!("invalid decimal '{s}': {e}"))),
    }
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

fn default_atomic_quote_valid_secs() -> u64 {
    120
}

fn default_settle_grace_secs() -> u64 {
    30
}

fn default_ticket_batch_size() -> usize {
    50
}

fn default_ticket_low_water() -> usize {
    50
}

fn default_split_poll_interval_secs() -> u64 {
    60
}

fn default_split_low_water_divisor() -> u32 {
    4
}

fn default_split_min_interval_secs() -> u64 {
    120
}

fn default_split_max_ops_per_hour() -> u32 {
    6
}

fn default_updates_poll_interval_secs() -> u64 {
    2
}

fn default_max_input_holdings() -> usize {
    100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_instrument_uses_wire_id_symbol() {
        // Wire-id divergence: the RPC serves a readable instrument_id with the
        // on-chain wire id in `symbol` (an opaque UUID). resolve_instrument must
        // return the WIRE id — legacy tokens (symbol == id) and CC→Amulet unchanged.
        let mut config = BaseConfig::test_minimal();
        config.populate_instruments_from_rpc(vec![
            Instrument {
                instrument_id: "ACME".into(),
                instrument_type: "fiat".into(),
                name: "ACME".into(),
                symbol: "0a1b2c3d-1111-4222-8333-444455556666".into(),
                registry: Some("issuer-1::1220aaaa".into()),
                ..Default::default()
            },
            Instrument {
                instrument_id: "USDCx".into(),
                instrument_type: "fiat".into(),
                name: "USD Coin".into(),
                symbol: "USDCx".into(),
                registry: Some("usdc-rep::12208115".into()),
                ..Default::default()
            },
            Instrument {
                instrument_id: "CC".into(),
                instrument_type: "token".into(),
                name: "Canton Coin".into(),
                symbol: "CC".into(),
                registry: Some("DSO::1220b143".into()),
                ..Default::default()
            },
        ]);

        let (acme_wire, acme_reg) = config.resolve_instrument("ACME");
        assert_eq!(acme_wire, "0a1b2c3d-1111-4222-8333-444455556666");
        assert_eq!(acme_reg, "issuer-1::1220aaaa");

        let (usdcx_wire, _) = config.resolve_instrument("USDCx");
        assert_eq!(usdcx_wire, "USDCx", "legacy token unchanged");

        let (cc_wire, _) = config.resolve_instrument("CC");
        assert_eq!(cc_wire, "Amulet", "CC translation unchanged");

        // Reverse direction: on-chain wire id → internal id.
        assert_eq!(
            config.internal_id_for_wire("0a1b2c3d-1111-4222-8333-444455556666"),
            Some("ACME".to_string())
        );
        // Identity (legacy) and unknown ids need no translation.
        assert_eq!(config.internal_id_for_wire("USDCx"), None);
        assert_eq!(config.internal_id_for_wire("ACME"), None, "idempotent on internal ids");
        assert_eq!(config.internal_id_for_wire("nonexistent"), None);
    }

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
        assert!(!agent.rfq_v2_only);
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
        assert_eq!(
            agent.liquidity_provider.as_ref().unwrap().min_notional_usd,
            0.0
        );
        assert_eq!(
            agent.markets[0].rfq.as_ref().unwrap().min_notional_usd,
            None
        );

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
        assert_eq!(
            agent.liquidity_provider.as_ref().unwrap().min_notional_usd,
            10.0
        );
        assert_eq!(
            agent.markets[0].rfq.as_ref().unwrap().min_notional_usd,
            Some(25.0)
        );
    }

    #[test]
    fn test_venue_overrides_parse() {
        // Omitted list → empty (no overrides), the deployed-toml default.
        let agent: AgentToml = toml::from_str("").unwrap();
        assert!(agent.venue_overrides.is_empty());

        // A full and a sparse entry; unset overlay fields must parse to None
        // (inherit the pair value), not to a default.
        let agent: AgentToml = toml::from_str(
            r#"
[[venue_overrides]]
venue = "walley"
branch = "main"
markets = ["HECTO-USDCx", "HECTO-CC"]

[venue_overrides.rfq]
enabled = true
min_quantity = "100"
max_quantity = "20000"
bid_spread_percent = 2.0
offer_spread_percent = 0.5
disable_overload_spread_widening = true
disable_depletion_spread_widening = true
quote_valid_secs = 30

[[venue_overrides]]
venue = "lattice"

[venue_overrides.rfq]
offer_spread_percent = 1.0
"#,
        )
        .unwrap();
        assert_eq!(agent.venue_overrides.len(), 2);

        let full = &agent.venue_overrides[0];
        assert_eq!(full.venue, "walley");
        assert_eq!(full.branch.as_deref(), Some("main"));
        assert_eq!(
            full.markets.as_deref(),
            Some(&["HECTO-USDCx".to_string(), "HECTO-CC".to_string()][..])
        );
        assert_eq!(full.rfq.enabled, Some(true));
        assert_eq!(full.rfq.min_quantity.as_deref(), Some("100"));
        assert_eq!(full.rfq.bid_spread_percent, Some(2.0));
        assert_eq!(full.rfq.disable_overload_spread_widening, Some(true));
        assert_eq!(full.rfq.quote_valid_secs, Some(30));
        assert!(!full.rfq.is_empty());

        let sparse = &agent.venue_overrides[1];
        assert_eq!(sparse.venue, "lattice");
        assert_eq!(sparse.branch, None, "omitted branch = any branch");
        assert_eq!(sparse.markets, None, "omitted markets = all markets");
        assert_eq!(sparse.rfq.offer_spread_percent, Some(1.0));
        assert_eq!(sparse.rfq.bid_spread_percent, None);
        assert_eq!(sparse.rfq.enabled, None);
        assert_eq!(sparse.rfq.disable_overload_spread_widening, None);
    }

    #[test]
    fn test_resolve_rfq_config_matching_and_layering() {
        let pair: RfqMarketConfig = serde_json::from_str(
            r#"{"min_quantity":"50","max_quantity":"10000","bid_spread_percent":2.5,"offer_spread_percent":0.5}"#,
        )
        .unwrap();
        let overrides: Vec<VenueOverride> = toml::from_str::<AgentToml>(
            r#"
# venue-wide (specificity 0): sets both spreads
[[venue_overrides]]
venue = "walley"
[venue_overrides.rfq]
bid_spread_percent = 4.0
offer_spread_percent = 4.0

# branch-scoped (specificity 1): tweaks ONE field on top
[[venue_overrides]]
venue = "walley"
branch = "main"
[venue_overrides.rfq]
offer_spread_percent = 1.0

# market-scoped for another venue
[[venue_overrides]]
venue = "lattice"
markets = ["CC-USDCx"]
[venue_overrides.rfq]
disable_overload_spread_widening = true
"#,
        )
        .unwrap()
        .venue_overrides;

        // No venue (V1 / legacy server) → borrowed pair config untouched.
        let r = resolve_rfq_config(&pair, &overrides, "CC-USDCx", None, None);
        assert!(matches!(r, std::borrow::Cow::Borrowed(_)));
        assert_eq!(r.bid_spread_percent, 2.5);

        // Unknown venue → borrowed pair config.
        let r = resolve_rfq_config(&pair, &overrides, "CC-USDCx", Some("supa"), None);
        assert!(matches!(r, std::borrow::Cow::Borrowed(_)));

        // Venue-wide match, no branch sent: only the specificity-0 entry
        // applies (branch-scoped needs the branch on the wire).
        let r = resolve_rfq_config(&pair, &overrides, "CC-USDCx", Some("walley"), None);
        assert!(matches!(r, std::borrow::Cow::Owned(_)));
        assert_eq!(r.bid_spread_percent, 4.0);
        assert_eq!(r.offer_spread_percent, 4.0);
        assert_eq!(r.min_quantity, "50", "unset overlay fields inherit the pair");

        // Branch sent: venue-wide applies first, branch-scoped overwrites the
        // one field it sets.
        let r = resolve_rfq_config(&pair, &overrides, "CC-USDCx", Some("walley"), Some("main"));
        assert_eq!(r.bid_spread_percent, 4.0, "kept from the venue-wide layer");
        assert_eq!(r.offer_spread_percent, 1.0, "branch layer wins");

        // Other branch: branch-scoped entry does not match.
        let r = resolve_rfq_config(&pair, &overrides, "CC-USDCx", Some("walley"), Some("beta"));
        assert_eq!(r.offer_spread_percent, 4.0);

        // Market scoping: lattice matches CC-USDCx only.
        let r = resolve_rfq_config(&pair, &overrides, "CC-USDCx", Some("lattice"), None);
        assert!(r.disable_overload_spread_widening);
        let r = resolve_rfq_config(&pair, &overrides, "CBTC-USDCx", Some("lattice"), None);
        assert!(matches!(r, std::borrow::Cow::Borrowed(_)));
    }

    /// EQUAL-specificity tie rule: stable sort keeps file order, the later
    /// entry applies later and wins. Pins the documented behavior against a
    /// routine sort_by_key -> sort_unstable_by_key "optimization", which
    /// would make live venue pricing implementation-defined.
    #[test]
    fn test_resolve_rfq_config_equal_specificity_later_entry_wins() {
        let pair: RfqMarketConfig = serde_json::from_str(
            r#"{"min_quantity":"50","max_quantity":"10000","bid_spread_percent":2.5,"offer_spread_percent":0.5}"#,
        )
        .unwrap();
        let overrides = toml::from_str::<AgentToml>(
            r#"
[[venue_overrides]]
venue = "walley"
[venue_overrides.rfq]
bid_spread_percent = 3.0
offer_spread_percent = 3.0

[[venue_overrides]]
venue = "walley"
[venue_overrides.rfq]
bid_spread_percent = 1.0
"#,
        )
        .unwrap()
        .venue_overrides;

        let r = resolve_rfq_config(&pair, &overrides, "CC-USDCx", Some("walley"), None);
        assert_eq!(r.bid_spread_percent, 1.0, "later same-specificity entry must win");
        assert_eq!(r.offer_spread_percent, 3.0, "field untouched by the later entry keeps the earlier layer");
    }

    #[test]
    fn test_venue_overrides_validation() {
        // Invalid venue slug (uppercase) must fail assemble-time validation.
        // Exercise the same predicate the validation uses.
        assert!(crate::auth::is_valid_venue_branch("walley"));
        assert!(crate::auth::is_valid_venue_branch("main"));
        assert!(!crate::auth::is_valid_venue_branch("Walley"));
        assert!(!crate::auth::is_valid_venue_branch(""));
        assert!(!crate::auth::is_valid_venue_branch("x"));
        assert!(!crate::auth::is_valid_venue_branch("-bad"));
    }

    #[test]
    fn test_rfq_v2_toml_defaults_and_parse() {
        // No [liquidity_provider.rfq_v2] / [markets.rfq.v2] → None
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
        assert!(agent.liquidity_provider.as_ref().unwrap().rfq_v2.is_none());
        assert!(agent.markets[0].rfq.as_ref().unwrap().v2.is_none());

        // Full sections; ticket_threshold_usd tolerant of quoted decimals
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"

[liquidity_provider.rfq_v2]
enabled = true
ticket_threshold_usd = "1000"

[[markets]]
market_id = "CC-USDCx"

[markets.rfq]
min_quantity = "5"
max_quantity = "1000"

[markets.rfq.v2]
enabled = true
denominations = ["25x20", "100x10"]
"#,
        )
        .unwrap();
        let v2 = agent
            .liquidity_provider
            .as_ref()
            .unwrap()
            .rfq_v2
            .as_ref()
            .unwrap();
        assert!(v2.enabled);
        assert_eq!(v2.atomic_quote_valid_secs, 120);
        assert_eq!(v2.settle_grace_secs, 30);
        assert_eq!(v2.ticket_threshold_usd, Some(1000.0));
        assert_eq!(v2.ticket_batch_size, 50);
        assert_eq!(v2.ticket_low_water, 50);
        assert_eq!(v2.split_poll_interval_secs, 60);
        assert_eq!(v2.updates_poll_interval_secs, 2);
        // split-storm guards default sensibly when absent from the config
        assert_eq!(v2.split_low_water_divisor, 4);
        assert_eq!(v2.split_min_interval_secs, 120);
        assert_eq!(v2.split_max_ops_per_hour, 6);
        let v2m = agent.markets[0].rfq.as_ref().unwrap().v2.as_ref().unwrap();
        assert!(v2m.enabled);
        assert_eq!(v2m.denominations, vec!["25x20", "100x10"]);
        assert_eq!(v2m.max_input_holdings, 100);
        // The net-tracker backstop is DERIVED, so raising the quote validity
        // cannot make it reverse the soft counts of quotes that are still open.
        assert_eq!(v2.stale_pending_after().as_secs(), 900);

        // ...and it outlives the quote at every plausible setting, including
        // ones far past the 900 s the backstop used to hard-code.
        for (valid, grace) in [(120, 30), (900, 300), (1800, 600), (3600, 3600)] {
            let c = RfqV2Config {
                atomic_quote_valid_secs: valid,
                settle_grace_secs: grace,
                ..Default::default()
            };
            let backstop = c.stale_pending_after().as_secs();
            assert!(
                backstop > valid + grace,
                "backstop {backstop}s would reverse a still-live quote \
                 ({valid}s validity + {grace}s grace)"
            );
        }

        // global per-instrument ladder map parses
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"

[liquidity_provider.rfq_v2]
enabled = true

[liquidity_provider.rfq_v2.denominations]
CC = ["100x10", "250x4"]
USDC = ["15x10"]
"#,
        )
        .unwrap();
        let v2 = agent
            .liquidity_provider
            .as_ref()
            .unwrap()
            .rfq_v2
            .as_ref()
            .unwrap();
        assert_eq!(v2.denominations["CC"], vec!["100x10", "250x4"]);
        assert_eq!(v2.denominations["USDC"], vec!["15x10"]);

        // split-storm guard knobs parse when present
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"

[liquidity_provider.rfq_v2]
split_low_water_divisor = 2
split_min_interval_secs = 300
split_max_ops_per_hour = 3
"#,
        )
        .unwrap();
        let v2 = agent
            .liquidity_provider
            .as_ref()
            .unwrap()
            .rfq_v2
            .as_ref()
            .unwrap();
        assert_eq!(v2.split_low_water_divisor, 2);
        assert_eq!(v2.split_min_interval_secs, 300);
        assert_eq!(v2.split_max_ops_per_hour, 3);

        // unquoted numeric threshold also parses
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"

[liquidity_provider.rfq_v2]
ticket_threshold_usd = 250.5
"#,
        )
        .unwrap();
        assert_eq!(
            agent
                .liquidity_provider
                .unwrap()
                .rfq_v2
                .unwrap()
                .ticket_threshold_usd,
            Some(250.5)
        );

        // top-level rfq_v2_only parses
        let agent: AgentToml = toml::from_str(
            r#"
rfq_v2_only = true

[liquidity_provider]
name = "LP test"
"#,
        )
        .unwrap();
        assert!(agent.rfq_v2_only);
    }

    /// Env-mutating assemble test. Single test fn so the process-global env
    /// is only touched from one thread; every scenario runs sequentially.
    #[test]
    fn test_rfq_v2_assemble_env_overrides_and_validation() {
        fn set(k: &str, v: &str) {
            unsafe { std::env::set_var(k, v) }
        }
        fn unset(k: &str) {
            unsafe { std::env::remove_var(k) }
        }

        // Required base env
        set("DSO", "dso::1220aa");
        set("PARTY_AGENT", "lp::1220bb");
        set(
            "PARTY_AGENT_PRIVATE_KEY",
            "EB92Q6V2a78t9ppqMuKLppyfzFgyYJciQEVHZKnXAhjEwVpx9aMbQN84SR4ceo3mbLUxQF7TLzaEujaTJnS7eRF",
        );
        set("ORDERBOOK_GRPC_URL", "https://example.test:443");
        set("SYNCHRONIZER_ID", "sync::1220cc");
        set("PARTY_SETTLEMENT_OPERATOR", "op::1220dd");
        set("NODE_NAME", "test-node");
        set(
            "LEDGER_SERVICE_PUBLIC_KEY",
            &bs58::encode([7u8; 32]).into_string(),
        );
        for k in [
            "RFQ_V2_ENABLED",
            "RFQ_V2_ONLY",
            "TICKET_THRESHOLD_USD",
            "TICKET_BATCH_SIZE",
            "ATOMIC_QUOTE_PRIVATE_KEY",
        ] {
            unset(k);
        }

        let market_v2_toml = r#"
[liquidity_provider]
name = "LP test"

[[markets]]
market_id = "CC-USDCx"

[markets.rfq]
min_quantity = "5"
max_quantity = "1000"

[markets.rfq.v2]
enabled = true
"#;

        // A: market v2 enabled without the LP-level switch → error
        let agent: AgentToml = toml::from_str(market_v2_toml).unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("liquidity_provider.rfq_v2"), "got: {err}");

        // B: RFQ_V2_ENABLED forces the LP switch; scalar env supplies the key;
        // TICKET_THRESHOLD_USD / TICKET_BATCH_SIZE override
        let kf = atomic_quote::gen_keypair().unwrap();
        set("RFQ_V2_ENABLED", "true");
        set("ATOMIC_QUOTE_PRIVATE_KEY", &kf.priv_scalar_hex);
        set("TICKET_THRESHOLD_USD", "250");
        set("TICKET_BATCH_SIZE", "77");
        let agent: AgentToml = toml::from_str(market_v2_toml).unwrap();
        let cfg = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap();
        let v2 = cfg
            .liquidity_provider
            .as_ref()
            .unwrap()
            .rfq_v2
            .as_ref()
            .unwrap();
        assert!(v2.enabled);
        assert_eq!(v2.ticket_threshold_usd, Some(250.0));
        assert_eq!(v2.ticket_batch_size, 77);
        let key = cfg.atomic_quote_key.as_ref().expect("quote key loaded");
        assert_eq!(key.pub_spki_hex, kf.pub_spki_hex);

        // C: disabled V2 loads no key even when the env var is set
        unset("RFQ_V2_ENABLED");
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"
"#,
        )
        .unwrap();
        let cfg = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap();
        assert!(cfg.atomic_quote_key.is_none());

        // D: max_input_holdings out of the 1..=100 protocol bound → error
        set("RFQ_V2_ENABLED", "true");
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"

[[markets]]
market_id = "CC-USDCx"

[markets.rfq]
min_quantity = "5"
max_quantity = "1000"

[markets.rfq.v2]
enabled = true
max_input_holdings = 125
"#,
        )
        .unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("max_input_holdings"), "got: {err}");

        // E: ticket_batch_size == 0 → error
        unset("TICKET_BATCH_SIZE");
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"

[liquidity_provider.rfq_v2]
enabled = true
ticket_batch_size = 0
"#,
        )
        .unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("ticket_batch_size"), "got: {err}");

        // F: atomic_quote_valid_secs == 0 → error
        let agent: AgentToml = toml::from_str(
            r#"
[liquidity_provider]
name = "LP test"

[liquidity_provider.rfq_v2]
enabled = true
atomic_quote_valid_secs = 0
"#,
        )
        .unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("atomic_quote_valid_secs"), "got: {err}");

        // G: enabled without the env key → error (env-only, no keyfile fallback)
        unset("ATOMIC_QUOTE_PRIVATE_KEY");
        let agent: AgentToml = toml::from_str(market_v2_toml).unwrap();
        let err = format!("{:#}", BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err());
        assert!(err.contains("ATOMIC_QUOTE_PRIVATE_KEY"), "got: {err}");

        // --- rfq_v2_only scenarios ---
        let only_toml = format!("rfq_v2_only = true\n{market_v2_toml}");

        // H: rfq_v2_only without a [liquidity_provider] section → error
        unset("RFQ_V2_ENABLED");
        let agent: AgentToml = toml::from_str("rfq_v2_only = true\n").unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("[liquidity_provider]"), "got: {err}");

        // I: rfq_v2_only with the LP-level V2 switch off → error
        let agent: AgentToml = toml::from_str(
            r#"
rfq_v2_only = true

[liquidity_provider]
name = "LP test"
"#,
        )
        .unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("rfq_v2].enabled"), "got: {err}");

        // J: rfq_v2_only with V2 enabled but no v2-enabled market → error
        set("RFQ_V2_ENABLED", "true");
        set("ATOMIC_QUOTE_PRIVATE_KEY", &kf.priv_scalar_hex);
        let agent: AgentToml = toml::from_str(
            r#"
rfq_v2_only = true

[liquidity_provider]
name = "LP test"

[liquidity_provider.rfq_v2]
enabled = true
"#,
        )
        .unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("at least one enabled market"), "got: {err}");

        // K: happy path — toml switch + enabled V2 + v2 market → Ok
        let agent: AgentToml = toml::from_str(&only_toml).unwrap();
        let cfg = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap();
        assert!(cfg.rfq_v2_only);

        // L: env RFQ_V2_ONLY=true over a toml that omits the field
        set("RFQ_V2_ONLY", "true");
        let agent: AgentToml = toml::from_str(market_v2_toml).unwrap();
        let cfg = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap();
        assert!(cfg.rfq_v2_only);

        // M: env RFQ_V2_ONLY=false disarms a toml `rfq_v2_only = true`
        // (validation then no longer applies, so this also passes without markets)
        set("RFQ_V2_ONLY", "false");
        let agent: AgentToml = toml::from_str(&only_toml).unwrap();
        let cfg = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap();
        assert!(!cfg.rfq_v2_only);

        // N: RFQ_V2_ONLY is LP-gated (RFQ_V2_ENABLED idiom) — with no
        // [liquidity_provider] section the env var is inert, so utility
        // commands on the load_or_defaults path don't trip the validation
        set("RFQ_V2_ONLY", "true");
        let agent: AgentToml = toml::from_str("").unwrap();
        let cfg = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap();
        assert!(!cfg.rfq_v2_only);

        // O: CLI overrides beat the env values; blank overrides are ignored; the
        // quote override satisfies RFQ V2 with the env var unset
        unset("RFQ_V2_ONLY");
        set("RFQ_V2_ENABLED", "true");
        set("ATOMIC_QUOTE_PRIVATE_KEY", &kf.priv_scalar_hex);
        let kf2 = atomic_quote::gen_keypair().unwrap();
        let (k2_b58, _) = crate::sign::generate_keypair();
        let agent: AgentToml = toml::from_str(market_v2_toml).unwrap();
        let cfg = BaseConfig::assemble(
            agent,
            ConfigOverrides {
                party: Some("other::1220ff".to_string()),
                private_key: Some(Zeroizing::new(k2_b58.clone())),
                quote_private_key: Some(Zeroizing::new(kf2.priv_scalar_hex.clone())),
            },
        )
        .unwrap();
        assert_eq!(cfg.party_id, "other::1220ff");
        assert_eq!(
            cfg.public_key_hex,
            get_public_key_hex(&decode_private_key(&k2_b58).unwrap())
        );
        assert_eq!(cfg.atomic_quote_key.as_ref().unwrap().pub_spki_hex, kf2.pub_spki_hex);

        let agent: AgentToml = toml::from_str(market_v2_toml).unwrap();
        let cfg = BaseConfig::assemble(
            agent,
            ConfigOverrides {
                party: Some("   ".to_string()),
                private_key: Some(Zeroizing::new(String::new())),
                quote_private_key: Some(Zeroizing::new(" ".to_string())),
            },
        )
        .unwrap();
        assert_eq!(cfg.party_id, "lp::1220bb");
        assert_eq!(cfg.atomic_quote_key.as_ref().unwrap().pub_spki_hex, kf.pub_spki_hex);

        unset("ATOMIC_QUOTE_PRIVATE_KEY");
        let agent: AgentToml = toml::from_str(market_v2_toml).unwrap();
        let cfg = BaseConfig::assemble(
            agent,
            ConfigOverrides {
                quote_private_key: Some(Zeroizing::new(kf2.priv_scalar_hex.clone())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(cfg.atomic_quote_key.as_ref().unwrap().pub_spki_hex, kf2.pub_spki_hex);
        let agent: AgentToml = toml::from_str(market_v2_toml).unwrap();
        let err = format!(
            "{:#}",
            BaseConfig::assemble(
                agent,
                ConfigOverrides {
                    quote_private_key: Some(Zeroizing::new("zz".to_string())),
                    ..Default::default()
                },
            )
            .unwrap_err()
        );
        assert!(err.contains("--quote-private-key"), "got: {err}");

        // cleanup
        for k in [
            "RFQ_V2_ENABLED",
            "RFQ_V2_ONLY",
            "TICKET_THRESHOLD_USD",
            "TICKET_BATCH_SIZE",
            "ATOMIC_QUOTE_PRIVATE_KEY",
        ] {
            unset(k);
        }
    }

    #[test]
    fn test_quote_key_from_short_scalar_hex() {
        let short = "01".repeat(24);
        let key = AtomicQuoteKey::from_scalar_hex(&short).unwrap();
        assert_eq!(key.scalar_hex().len(), 64);
        assert!(key.scalar_hex().starts_with(&"0".repeat(16)));
        assert!(key.scalar_hex().ends_with(&short));
        assert!(AtomicQuoteKey::from_scalar_hex(&"01".repeat(10)).is_err());
    }

    #[test]
    fn test_quote_key_from_scalar_hex() {
        let kf = atomic_quote::gen_keypair().unwrap();
        let key = AtomicQuoteKey::from_scalar_hex(&kf.priv_scalar_hex).unwrap();
        assert_eq!(key.pub_spki_hex, kf.pub_spki_hex);
        assert_eq!(*key.scalar_hex(), kf.priv_scalar_hex.to_lowercase());
        assert_eq!(hex::encode(key.scalar().as_slice()), kf.priv_scalar_hex.to_lowercase());
        assert_eq!(format!("{key:?}"), format!("AtomicQuoteKey {{ pub_spki_hex: {:?}, .. }}", kf.pub_spki_hex));
        assert!(AtomicQuoteKey::from_scalar_hex("zz").is_err());
    }

    /// Non-env mirror: the same rfq_v2_only requirements enforced by
    /// `validate_v2` for `for_party` embedders.
    #[test]
    fn test_rfq_v2_only_validate_v2_mirror() {
        let mut config = BaseConfig::test_minimal();
        config.rfq_v2_only = true;

        // No LP section
        let err = config.validate_v2().unwrap_err().to_string();
        assert!(err.contains("[liquidity_provider]"), "got: {err}");

        // LP present but V2 switch off
        config.liquidity_provider = Some(LiquidityProviderConfig {
            name: "LP test".to_string(),
            max_concurrent_rfqs: default_max_concurrent_rfqs(),
            default_quote_valid_secs: default_quote_valid_secs(),
            min_notional_usd: 0.0,
            rfq_v2: None,
        });
        let err = config.validate_v2().unwrap_err().to_string();
        assert!(err.contains("rfq_v2].enabled"), "got: {err}");

        // V2 enabled (+ key so the shared quote-key check passes) but no v2 market
        config.liquidity_provider.as_mut().unwrap().rfq_v2 = Some(RfqV2Config {
            enabled: true,
            ..RfqV2Config::default()
        });
        let kf = atomic_quote::gen_keypair().unwrap();
        config.set_atomic_quote_scalar(&kf.priv_scalar_hex).unwrap();
        let err = config.validate_v2().unwrap_err().to_string();
        assert!(err.contains("at least one enabled market"), "got: {err}");

        // Add a v2-enabled market → Ok
        let market: MarketConfig = toml::from_str(
            r#"
market_id = "CC-USDCx"

[rfq]
min_quantity = "5"
max_quantity = "1000"

[rfq.v2]
enabled = true
"#,
        )
        .unwrap();
        config.markets = vec![market];
        config.recompute_deadline_windows();
        config.validate_v2().unwrap();
    }
}
