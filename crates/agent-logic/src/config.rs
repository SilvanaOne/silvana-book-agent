//! Base configuration for the orderbook agent
//!
//! Assembled from three sources:
//! 1. `.env` — shared infrastructure env vars (URLs, party IDs, secrets)
//! 2. `configuration.toml` — shared token config (registries, Canton Coin)
//! 3. `agent.toml` — agent-specific settings (markets, polling, JWT)
//!
//! This is the shared `BaseConfig` used by both the local agent and the cloud agent.
//! The local agent wraps this with a `Config` that adds ledger API URLs.

use anyhow::{Context, Result, anyhow, bail};
use orderbook_proto::orderbook::Instrument;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Display;
use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use zeroize::Zeroize;

use crate::auth::get_public_key_hex;
use crate::secret::{Secret, Zeroizing};

pub use crate::strategy::config::{
    ArbitrageConfig, CommonParams, GridParams, MarketMakerConfig, MeanReversionConfig, MomentumConfig,
    NoiseTakerConfig, RebalancerConfig, StrategyConfig, StrategyMode, TradeSide, TwapConfig,
};

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
    /// Reject quoting/committing from balances older than this; 0 disables.
    #[serde(default = "default_balance_stale_after_secs")]
    balance_stale_after_secs: u64,
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
    /// Prepaid CC held per settlement for its own transactions (`AGENT_SETTLEMENT_TRAFFIC_CC`).
    pub settlement_traffic_cc: f64,
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
    /// Reject quoting/committing from balances older than this; 0 disables.
    pub balance_stale_after_secs: u64,
    pub markets: Vec<MarketConfig>,

    /// `[markets.strategy]` tables by market id; a market without one runs the grid.
    pub strategies: BTreeMap<String, StrategyConfig>,

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
    /// CANCELLED on encounter (see `abort_v1_settlement`), as this mode takes
    /// no V1 steps; ones this agent already allocated for are left to settle
    /// via the operator, and an RFQ trade another client of this party takes
    /// is left alone. Already-paid counterparty fees are not refunded — still
    /// prefer flipping during a quiet V1 window.
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

    /// `CANTON_CHAIN`: the network; control supports `devnet` and `mainnet`.
    pub canton_chain: Option<String>,
    /// `PARTY_FEE`: the platform fee party, pinned locally.
    pub party_fee: Option<String>,
    /// `PARTY_PREPAID_TRAFFIC`: the prepaid traffic party, pinned locally.
    pub party_prepaid_traffic: Option<String>,
    /// agent.toml `[control]`: what a linked agent does with pending controller work.
    pub control: ControlSettings,
    /// agent.toml `[vetted_packages]`: checked before every signature when `selected`.
    pub vetted_packages: VettedPackages,
    /// agent.toml `[verify]`: how the verifier's newer checks act while not linked.
    pub verify: VerifySettings,
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
            scalar: Secret::seal(&mut bytes)?,
        })
    }

    /// Sealed scalar, exposed briefly for signing.
    pub fn scalar(&self) -> Result<crate::secret::Exposed<32>, crate::secret::SecretError> {
        self.scalar.expose()
    }

    /// Lowercase hex of the scalar; the returned string is zeroed on drop.
    pub fn scalar_hex(&self) -> Result<Zeroizing<String>, crate::secret::SecretError> {
        Ok(Zeroizing::new(hex::encode(self.scalar.expose()?.as_slice())))
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
    pub fn test_minimal() -> Result<Self> {
        Ok(Self {
            orderbook_grpc_url: String::new(),
            synchronizer_id: String::new(),
            party_id: "test-party".to_string(),
            private_key: Secret::seal(&mut [0u8; 32])?,
            public_key_hex: String::new(),
            settlement_operator: String::new(),
            fee_reserve_cc: 5.0,
            settlement_traffic_cc: 1.2,
            merge_threshold: None,
            merge_max_amulets: 0,
            merge_poll_interval_sec: 600,
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
            balance_stale_after_secs: 120,
            markets: Vec::new(),
            strategies: BTreeMap::new(),
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
            canton_chain: None,
            party_fee: None,
            party_prepaid_traffic: None,
            control: ControlSettings::default(),
            vetted_packages: VettedPackages::default(),
            verify: VerifySettings::default(),
        })
    }
}

impl BaseConfig {
    /// The signer for the agent key.
    pub fn signer(&self) -> Result<crate::signer::AgentSigner> {
        crate::signer::resolve(&self.private_key)
    }

    fn jwt_request(&self) -> crate::signer::JwtRequest<'_> {
        crate::signer::JwtRequest {
            sub: &self.party_id,
            role: &self.role,
            ttl_secs: self.token_ttl_secs,
            node_name: Some(self.node_name.as_str()),
            venue_branch: None,
        }
    }

    /// A fresh JWT for the configured party, without a venue branch.
    pub fn jwt(&self) -> Result<String> {
        self.signer()?.sign_jwt(&self.jwt_request())
    }

    /// A token for the control services of `CANTON_CHAIN` (at most 900 s) and the head it carries.
    pub fn control_jwt(&self) -> Result<(String, Option<agent_control::HeadRef>)> {
        let network = match crate::control_store::chain_network(self.canton_chain.as_deref()) {
            crate::control_store::ChainNetwork::Mapped(network) => network,
            _ => anyhow::bail!("a control token needs CANTON_CHAIN=devnet or CANTON_CHAIN=mainnet"),
        };
        self.signer()?.sign_control_jwt(&self.jwt_request(), network)
    }

    /// A control token (at most 900 s) claiming `head`, the head after an append;
    /// `store` must hold it as its pending entry or in its log.
    pub fn control_jwt_for_head(
        &self,
        store: &crate::control_store::ControlStore,
        lock: &crate::control_store::ControlLock,
        head: &agent_control::HeadRef,
    ) -> Result<String> {
        self.signer()?
            .sign_control_jwt_for_head(&self.jwt_request(), store, lock, head)
    }

    /// The locally pinned platform parties (fee, prepaid traffic) that are set.
    pub fn platform_parties(&self) -> Vec<&str> {
        [self.party_fee.as_deref(), self.party_prepaid_traffic.as_deref()]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// A set, non-blank env var, trimmed.
fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// A pinned platform party from env `name`; a set value must look like a party id.
pub fn pinned_party_env(name: &str) -> Result<Option<String>> {
    pinned_party(name, optional_env(name))
}

fn pinned_party(name: &str, value: Option<String>) -> Result<Option<String>> {
    let Some(party) = value else {
        return Ok(None);
    };
    if !party.contains("::") || party.chars().any(char::is_whitespace) {
        return Err(anyhow!("{name} is not a party id: {party:?}"));
    }
    Ok(Some(party))
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
        let strategies = crate::strategy::config::parse_strategies(&agent_toml_str)
            .with_context(|| format!("Failed to parse {}", agent_toml_path.as_ref().display()))?;
        let sections = parse_control_sections(&agent_toml_str)
            .with_context(|| format!("Failed to parse {}", agent_toml_path.as_ref().display()))?;
        Self::assemble_with(agent, strategies, sections, overrides)
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
        let (agent, strategies, sections): (AgentToml, _, _) = if agent_toml_path.as_ref().exists() {
            let s = fs::read_to_string(agent_toml_path.as_ref()).with_context(|| {
                format!("Failed to read {}", agent_toml_path.as_ref().display())
            })?;
            let parse_err = || format!("Failed to parse {}", agent_toml_path.as_ref().display());
            (
                toml::from_str(&s).with_context(parse_err)?,
                crate::strategy::config::parse_strategies(&s).with_context(parse_err)?,
                parse_control_sections(&s).with_context(parse_err)?,
            )
        } else {
            // Empty string round-trips to AgentToml with all serde defaults.
            (
                toml::from_str("").context("AgentToml serde defaults must parse")?,
                BTreeMap::new(),
                ControlSections::default(),
            )
        };
        Self::assemble_with(agent, strategies, sections, overrides)
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
    /// SHOULD call [`BaseConfig::validate_v2`] before running RFQ V2 and
    /// [`BaseConfig::validate_instrument_registry`] before settling — the
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
        let private_key = Secret::seal(&mut private_key_bytes)?;
        let ledger_service_public_key = decode_public_key(ledger_service_public_key_base58)?;
        Ok(Self {
            orderbook_grpc_url: orderbook_grpc_url.to_string(),
            synchronizer_id: synchronizer_id.to_string(),
            party_id: party_id.to_string(),
            private_key,
            public_key_hex,
            settlement_operator: settlement_operator.to_string(),
            fee_reserve_cc: 5.0,
            settlement_traffic_cc: 1.2,
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
            balance_stale_after_secs: default_balance_stale_after_secs(),
            markets: Vec::new(),
            strategies: BTreeMap::new(),
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
            canton_chain: None,
            party_fee: None,
            party_prepaid_traffic: None,
            control: ControlSettings::default(),
            vetted_packages: VettedPackages::default(),
            verify: VerifySettings::default(),
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
    /// after setting `markets` / `liquidity_provider` / the quote key. Also
    /// runs the range checks of [`BaseConfig::validate_ranges`].
    pub fn validate_v2(&self) -> Result<()> {
        self.validate_ranges()?;
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
                check_market_quote_valid_secs(
                    &market.market_id,
                    v2m,
                    self.liquidity_provider.as_ref().and_then(|lp| lp.rfq_v2.as_ref()),
                )?;
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
            validate_rfq_v2_ranges(v2)?;
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
    #[cfg(test)]
    fn assemble(agent: AgentToml, overrides: ConfigOverrides) -> Result<Self> {
        Self::assemble_with(agent, BTreeMap::new(), ControlSections::default(), overrides)
    }

    /// [`BaseConfig::assemble`] with the strategy tables and control sections from the
    /// second TOML passes.
    fn assemble_with(
        mut agent: AgentToml,
        strategies: BTreeMap<String, StrategyConfig>,
        sections: ControlSections,
        overrides: ConfigOverrides,
    ) -> Result<Self> {
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
        if sections.controller.as_ref().is_some_and(|c| c.agent_party.as_str() == party_id.trim()) {
            bail!("[controller] names this agent's own party: a controller key never controls the agent it runs beside");
        }

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
        // A mismatched pair is only rejected once the server sees the token, so
        // check it here where the error can name both sides.
        let public_key_bytes: [u8; 32] = hex::decode(&public_key_hex)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| anyhow!("could not derive the public key for party {party_id}"))?;
        if party_matches_public_key(&party_id, &public_key_bytes) == Some(false) {
            return Err(anyhow!(
                "the private key does not belong to party {party_id} \
                 (its public key is {public_key_hex}); check --party and --private-key"
            ));
        }
        let private_key = Secret::seal(&mut private_key_bytes)?;
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

        let fee_reserve_cc: f64 = env_or("AGENT_FEE_RESERVE_CC", 5.0)?;
        let settlement_traffic_cc: f64 = env_or("AGENT_SETTLEMENT_TRAFFIC_CC", 1.2)?;

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
        let merge_poll_interval_sec: u64 = env_or("MERGE_POLL_INTERVAL_SEC", 600)?;

        let settlement_thread_count: usize = env_or("SETTLEMENT_THREAD_COUNT", 25)?;

        // Process-wide knobs read by background modules; checked here so a bad
        // value stops startup instead of falling back.
        crate::forecast::poll_secs_from_env()?;
        crate::ledger_health::cooldown_secs_from_env()?;

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

        let liquidity_margin: f64 = env_or("LIQUIDITY_MARGIN", 1.1)?;
        let flow_ema_window_hours: f64 = env_or("FLOW_EMA_WINDOW_HOURS", 4.0)?;
        let depletion_max_hours: f64 = env_or("DEPLETION_COEFF_MAX_HOURS", 12.0)?;
        let depletion_min_hours: f64 = env_or("DEPLETION_COEFF_MIN_HOURS", 1.0)?;

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
                check_market_quote_valid_secs(
                    &market.market_id,
                    v2m,
                    agent.liquidity_provider.as_ref().and_then(|lp| lp.rfq_v2.as_ref()),
                )?;
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
        for (n, ov) in (1usize..).zip(agent.venue_overrides.iter()) {
            if !crate::auth::is_valid_venue_branch(&ov.venue) {
                return Err(anyhow!(
                    "[[venue_overrides]] #{}: venue '{}' is not a valid slug \
                     (^[a-z0-9][a-z0-9-]{{1,19}}$) — it must equal the server's \
                     swap-venue name (AtomicRfqRequest.venue_name)",
                    n,
                    ov.venue
                ));
            }
            if let Some(ref b) = ov.branch {
                if !crate::auth::is_valid_venue_branch(b) {
                    return Err(anyhow!(
                        "[[venue_overrides]] #{} (venue '{}'): branch '{}' is not a \
                         valid slug (^[a-z0-9][a-z0-9-]{{1,19}}$)",
                        n,
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
                        n,
                        ov.venue
                    ));
                }
                for m in markets {
                    if !agent.markets.iter().any(|mc| &mc.market_id == m) {
                        tracing::warn!(
                            "[[venue_overrides]] #{} (venue '{}'): market '{}' is not in \
                             [[markets]] — that scope entry can never match",
                            n,
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
                                n,
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
                    n,
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
                        n,
                        ov.venue,
                        s
                    )
                })?),
                None => None,
            };
            let max = match &ov.rfq.max_quantity {
                Some(s) => Some(
                    s.parse::<f64>()
                        .ok()
                        .filter(|m| m.is_finite() && *m > 0.0)
                        .ok_or_else(|| {
                            anyhow!(
                                "[[venue_overrides]] #{} (venue '{}'): max_quantity '{}' is not \
                                 a positive number",
                                n,
                                ov.venue,
                                s
                            )
                        })?,
                ),
                None => None,
            };
            if let (Some(min), Some(max)) = (min, max) {
                if min > max {
                    return Err(anyhow!(
                        "[[venue_overrides]] #{} (venue '{}'): min_quantity {} > \
                         max_quantity {}",
                        n,
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
            validate_rfq_v2_ranges(v2)?;
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

        let config = BaseConfig {
            orderbook_grpc_url,
            synchronizer_id,
            party_id,
            private_key,
            public_key_hex,
            settlement_operator,
            fee_reserve_cc,
            settlement_traffic_cc,
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
            balance_stale_after_secs: agent.balance_stale_after_secs,
            markets: agent.markets,
            strategies,
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
            canton_chain: optional_env(crate::control_store::CHAIN_ENV),
            party_fee: pinned_party_env("PARTY_FEE")?,
            party_prepaid_traffic: pinned_party_env("PARTY_PREPAID_TRAFFIC")?,
            control: sections.control,
            vetted_packages: sections.vetted_packages,
            verify: sections.verify,
        };
        config.validate_ranges()?;
        Ok(config)
    }

    /// Range checks for values that size pools or feed timers, deadlines and
    /// timeouts: out of range is a startup error, never a silent clamp.
    pub fn validate_ranges(&self) -> Result<()> {
        check_range(
            "settlement_thread_count (SETTLEMENT_THREAD_COUNT)",
            self.settlement_thread_count,
            1..=crate::sync::MAX_PERMITS,
        )?;
        check_positive("merge_poll_interval_sec (MERGE_POLL_INTERVAL_SEC)", self.merge_poll_interval_sec)?;
        check_range("token_ttl_secs", self.token_ttl_secs, 60..=86_400)?;
        check_range("connection_timeout_secs", self.connection_timeout_secs, 1..=300)?;
        check_range("request_timeout_secs", self.request_timeout_secs, 1..=3600)?;
        check_range("canton_op_timeout_secs", self.canton_op_timeout_secs, 1..=86_400)?;
        for (name, value) in [
            ("fee_reserve_cc (AGENT_FEE_RESERVE_CC)", self.fee_reserve_cc),
            ("settlement_traffic_cc (AGENT_SETTLEMENT_TRAFFIC_CC)", self.settlement_traffic_cc),
            ("liquidity_margin (LIQUIDITY_MARGIN)", self.liquidity_margin),
            ("flow_ema_window_hours (FLOW_EMA_WINDOW_HOURS)", self.flow_ema_window_hours),
            ("depletion_max_hours (DEPLETION_COEFF_MAX_HOURS)", self.depletion_max_hours),
            ("depletion_min_hours (DEPLETION_COEFF_MIN_HOURS)", self.depletion_min_hours),
        ] {
            if !value.is_finite() {
                bail!("{name}={value} must be a finite number");
            }
        }
        if self.settlement_traffic_cc < 0.0 {
            bail!(
                "settlement_traffic_cc (AGENT_SETTLEMENT_TRAFFIC_CC)={} must not be negative",
                self.settlement_traffic_cc
            );
        }
        for market in &self.markets {
            if let Some(rfq) = &market.rfq {
                // Parsed exactly as the quoting path parses it
                let max = rfq.max_quantity.parse::<f64>().ok();
                if !max.is_some_and(|m| m.is_finite() && m > 0.0) {
                    bail!(
                        "Market {}: [markets.rfq] max_quantity '{}' must be a positive number",
                        market.market_id,
                        rfq.max_quantity
                    );
                }
            }
        }
        if let Some(v2) = self.liquidity_provider.as_ref().and_then(|lp| lp.rfq_v2.as_ref()) {
            validate_rfq_v2_ranges(v2)?;
        }
        crate::strategy::config::validate_strategies(self)
    }

    /// The market's strategy; the grid when it has no `[markets.strategy]` table.
    pub fn strategy_for(&self, market_id: &str) -> &StrategyConfig {
        self.strategies.get(market_id).unwrap_or(&crate::strategy::config::GRID)
    }

    /// Get list of enabled markets
    pub fn enabled_markets(&self) -> Vec<&MarketConfig> {
        self.markets.iter().filter(|m| m.enabled).collect()
    }

    /// Populate `cc_token_id`, `onboarded_registries` and `instrument_registries`
    /// from instruments fetched over the orderbook-rpc `GetInstruments` endpoint.
    /// Call this once at startup; settlement logic then needs
    /// [`BaseConfig::validate_instrument_registry`] to pass.
    ///
    /// Canton Coin is the instrument on the DSO registry (falling back to the
    /// first `instrument_type == "token"`); its id drives the CC → Amulet
    /// translation. Every other instrument simply maps id → registry.
    pub fn populate_instruments_from_rpc(&mut self, instruments: Vec<Instrument>) {
        let mut instrument_registries: HashMap<String, String> = HashMap::new();
        let mut instrument_wire_ids: HashMap<String, String> = HashMap::new();
        let mut cc_token_id: Option<String> = None;
        let mut first_token_id: Option<String> = None;
        let mut onboarded_registries: HashSet<String> = HashSet::new();

        for inst in instruments {
            let registry = inst.registry.clone().unwrap_or_default();
            if !registry.is_empty() {
                instrument_registries.insert(inst.instrument_id.clone(), registry.clone());
                onboarded_registries.insert(registry.clone());
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
            // CC is the instrument on the DSO registry; `instrument_type` is
            // not unique, so it only serves as a fallback below.
            if !self.dso_party.is_empty() && registry == self.dso_party && cc_token_id.is_none() {
                cc_token_id = Some(inst.instrument_id.clone());
            }
            if inst.instrument_type == "token" && first_token_id.is_none() {
                first_token_id = Some(inst.instrument_id.clone());
            }
        }

        if cc_token_id.is_none() {
            if self.dso_party.is_empty() {
                cc_token_id = first_token_id;
            } else if let Some(fallback) = first_token_id {
                tracing::warn!(
                    "No instrument is registered under the DSO party; falling back to '{}' for CC",
                    fallback
                );
                cc_token_id = Some(fallback);
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
    /// An unknown instrument gets an empty registry: display and accounting only;
    /// anything that checks an admin uses [`BaseConfig::try_resolve_instrument`].
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

    /// [`BaseConfig::resolve_instrument`] for verification: no registry admin is an
    /// error, never an empty string. Canton Coin falls back to the DSO party.
    pub fn try_resolve_instrument(&self, instrument_id: &str) -> Result<(String, String)> {
        let is_cc = Some(instrument_id) == self.cc_token_id.as_deref();
        let recorded = self
            .instrument_registries
            .get(instrument_id)
            .filter(|registry| !registry.trim().is_empty());
        let admin = match recorded {
            Some(registry) => registry.clone(),
            None if is_cc && !self.dso_party.trim().is_empty() => self.dso_party.clone(),
            None => bail!(
                "{UNRESOLVED_INSTRUMENT}: instrument '{instrument_id}' has no registry admin among \
                 the {} loaded instruments (delisted, not visible to this party, or the registry \
                 did not load)",
                self.instrument_wire_ids.len()
            ),
        };
        let (on_chain_id, _) = self.resolve_instrument(instrument_id);
        Ok((on_chain_id, admin))
    }

    /// Fails unless the registry can back settlement verification: instruments
    /// listed, registries recorded, and Canton Coin resolvable.
    pub fn validate_instrument_registry(&self) -> Result<()> {
        if self.instrument_wire_ids.is_empty() {
            bail!("orderbook-rpc listed no instruments");
        }
        if !self.instrument_registries.values().any(|r| !r.trim().is_empty()) {
            bail!(
                "none of the {} listed instruments carries a registry party",
                self.instrument_wire_ids.len()
            );
        }
        let cc = self.cc_token_id.as_deref().ok_or_else(|| {
            anyhow!(
                "no Canton Coin instrument: none is registered under the DSO party and none \
                 has type 'token'"
            )
        })?;
        self.try_resolve_instrument(cc).map(|_| ())
    }

    /// Resolve an instrument given EITHER its internal id or its wire id.
    /// `wire_id` is `None` when neither key space knows the input.
    pub fn resolve_instrument_or_wire(&self, input: &str) -> ResolvedInstrument {
        if Some(input) == self.cc_token_id.as_deref() {
            return ResolvedInstrument {
                wire_id: Some("Amulet".to_string()),
                registry: self.instrument_registries.get(input).cloned(),
            };
        }
        if let Some(wire) = self.instrument_wire_ids.get(input) {
            return ResolvedInstrument {
                wire_id: Some(wire.clone()),
                registry: self.instrument_registries.get(input).cloned(),
            };
        }
        // Not an internal id — try it as a wire id, which is what every
        // pre-existing invocation supplies.
        if let Some((internal, _)) = self
            .instrument_wire_ids
            .iter()
            .find(|(_, wire)| wire.as_str() == input)
        {
            return ResolvedInstrument {
                wire_id: Some(input.to_string()),
                registry: self.instrument_registries.get(internal).cloned(),
            };
        }
        ResolvedInstrument { wire_id: None, registry: None }
    }
}

/// Carried by every [`BaseConfig::try_resolve_instrument`] error, so callers can
/// tell an unresolvable instrument from other failures.
pub const UNRESOLVED_INSTRUMENT: &str = "unresolved-instrument";

/// Outcome of [`BaseConfig::resolve_instrument_or_wire`].
#[derive(Debug, Clone, Default)]
pub struct ResolvedInstrument {
    /// On-chain wire id, or `None` when the input matched no known instrument.
    pub wire_id: Option<String>,
    /// Registrar party, when the matched row carries one.
    pub registry: Option<String>,
}

// ============================================================================
// Helpers
// ============================================================================

/// `value` must lie in `range`.
fn check_range<T: PartialOrd + Display>(name: &str, value: T, range: RangeInclusive<T>) -> Result<()> {
    if range.contains(&value) {
        return Ok(());
    }
    let (lo, hi) = range.into_inner();
    bail!("{name}={value} is outside {lo}..={hi}")
}

/// A market's `[markets.rfq.v2] atomic_quote_valid_secs` lies between the
/// minimum and the LP-level window (the cap when no LP-level config is set).
fn check_market_quote_valid_secs(
    market_id: &str,
    v2m: &RfqV2MarketConfig,
    lp_v2: Option<&RfqV2Config>,
) -> Result<()> {
    let Some(secs) = v2m.atomic_quote_valid_secs else {
        return Ok(());
    };
    let max = lp_v2.map_or(RFQ_V2_MAX_QUOTE_VALID_SECS, |v2| v2.atomic_quote_valid_secs);
    check_range(
        &format!("Market {market_id}: [markets.rfq.v2] atomic_quote_valid_secs"),
        secs,
        RFQ_V2_MIN_MARKET_QUOTE_VALID_SECS..=max,
    )
}

/// `[liquidity_provider.rfq_v2]` values that feed timers and quote validity.
fn validate_rfq_v2_ranges(v2: &RfqV2Config) -> Result<()> {
    check_range("[liquidity_provider.rfq_v2] ticket_batch_size", v2.ticket_batch_size, 1..=1000)?;
    check_range(
        "[liquidity_provider.rfq_v2] atomic_quote_valid_secs",
        v2.atomic_quote_valid_secs,
        1..=RFQ_V2_MAX_QUOTE_VALID_SECS,
    )?;
    check_range(
        "[liquidity_provider.rfq_v2] settle_grace_secs",
        v2.settle_grace_secs,
        0..=RFQ_V2_MAX_SETTLE_GRACE_SECS,
    )?;
    check_positive("[liquidity_provider.rfq_v2] updates_poll_interval_secs", v2.updates_poll_interval_secs)?;
    check_positive("[liquidity_provider.rfq_v2] split_poll_interval_secs", v2.split_poll_interval_secs)
}

/// A poll interval of 0 would spin.
fn check_positive(name: &str, value: u64) -> Result<()> {
    if value == 0 {
        bail!("{name} must be at least 1");
    }
    Ok(())
}

/// Env var `name` parsed as `T`; unset or blank gives `default`, and a set
/// value that does not parse is an error.
fn env_or<T>(name: &str, default: T) -> Result<T>
where
    T: FromStr,
    T::Err: Display,
{
    match std::env::var(name) {
        Ok(raw) => parse_env_value(name, Some(&raw), default),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(e) => bail!("{name}: {e}"),
    }
}

fn parse_env_value<T>(name: &str, raw: Option<&str>, default: T) -> Result<T>
where
    T: FromStr,
    T::Err: Display,
{
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(default),
        Some(s) => s.parse().map_err(|e| anyhow!("{name}='{s}' is not valid: {e}")),
    }
}

/// Decode base58 Ed25519 public key to 32 bytes
/// Canton derives a party's namespace as `1220` + hex of
/// `SHA256(0x0000000C || public_key)`. Returns `None` when the party id does
/// not carry a namespace in that form, so nothing can be concluded.
pub fn party_matches_public_key(party_id: &str, public_key: &[u8; 32]) -> Option<bool> {
    use sha2::{Digest, Sha256};
    let namespace = party_id.split("::").nth(1)?;
    let expected = namespace.strip_prefix("1220")?;
    if expected.len() != 64 {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update([0x00, 0x00, 0x00, 0x0C]);
    hasher.update(public_key);
    Some(hex::encode(hasher.finalize()).eq_ignore_ascii_case(expected))
}

/// The Canton namespace of `public_key`: `1220` + hex of `SHA256(0x0000000C || key)`.
pub fn key_namespace(public_key: &[u8; 32]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update([0x00, 0x00, 0x00, 0x0C]);
    hasher.update(public_key);
    format!("1220{}", hex::encode(hasher.finalize()))
}

/// Refuses `party_id` unless its namespace is derived from `public_key`; a party
/// id without a `1220` namespace is refused too.
pub fn require_party_matches_key(party_id: &str, public_key: &[u8; 32]) -> Result<()> {
    match party_matches_public_key(party_id, public_key) {
        Some(true) => Ok(()),
        Some(false) => Err(anyhow!(
            "party {party_id} does not belong to this key (public key {})",
            hex::encode(public_key)
        )),
        None => Err(anyhow!("party {party_id} has no namespace that can be checked against this key")),
    }
}

pub fn decode_public_key(base58_key: &str) -> Result<[u8; 32]> {
    let key_bytes = bs58::decode(base58_key)
        .into_vec()
        .context("Invalid base58 public key")?;

    let len = key_bytes.len();
    key_bytes
        .try_into()
        .map_err(|_| anyhow!("Public key must be exactly 32 bytes, got {len}"))
}

/// Decode base58 Ed25519 private key to 32-byte seed
pub fn decode_private_key(base58_key: &str) -> Result<[u8; 32]> {
    let key_bytes = Zeroizing::new(
        bs58::decode(base58_key.trim())
            .into_vec()
            .context("Invalid base58 private key")?,
    );

    let seed = key_bytes.first_chunk::<32>().ok_or_else(|| {
        anyhow!(
            "Private key too short: expected at least 32 bytes, got {}",
            key_bytes.len()
        )
    })?;
    Ok(*seed)
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Refuse the LP's offer side on this market: requests where the user buys
    /// the base are rejected as unsupported. Not per-venue.
    #[serde(default)]
    pub disable_offer_side: bool,
    /// Refuse the LP's bid side on this market: requests where the user sells
    /// the base are rejected as unsupported. Not per-venue.
    #[serde(default)]
    pub disable_bid_side: bool,
    /// false = RFQ V2 only on this market: V1 requests are refused, since they
    /// carry no requesting party for the trailing-net term. Not per-venue.
    #[serde(default = "default_enabled")]
    pub v1_enabled: bool,
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
        u8::from(self.branch.is_some()).saturating_add(u8::from(self.markets.is_some()))
    }
}

/// Sparse all-`Option` mirror of [`RfqMarketConfig`]'s venue-effective
/// scalars: a set field replaces the pair value, an unset one inherits it.
/// Deliberately excluded — fields with NO effect on any venue-carrying
/// request (offering them would be dead config that deceives the operator):
/// `v2` (denomination ladders, structural), `min_notional_usd` (the floor is
/// V1-only, and V1 carries no venue), `allocate_before_secs` /
/// `settle_before_secs` (consumed only by the V1 quote message; V2 deadlines
/// come from the atomic-quote globals), `disable_offer_side` /
/// `disable_bid_side` (a closed side is a property of the pair, not of who is
/// asking — a venue must not be able to re-open it).
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
    /// Signed-quote validity for this market, in seconds; may only shorten the
    /// LP-level `atomic_quote_valid_secs`. None = the LP-level window.
    #[serde(default)]
    pub atomic_quote_valid_secs: Option<u64>,
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

/// Largest accepted `atomic_quote_valid_secs`.
pub const RFQ_V2_MAX_QUOTE_VALID_SECS: u64 = 3600;
/// Largest accepted `settle_grace_secs`.
pub const RFQ_V2_MAX_SETTLE_GRACE_SECS: u64 = 3600;
/// Smallest accepted per-market `atomic_quote_valid_secs`: the longest
/// confirm-time split wait plus the margins a taker needs to settle.
pub const RFQ_V2_MIN_MARKET_QUOTE_VALID_SECS: u64 = 60;

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
// [control] and [vetted_packages] (a strict second TOML pass)
// ============================================================================

/// Largest `[control] swap_max_deviation_bps`: 100 %.
pub const MAX_SWAP_DEVIATION_BPS: u32 = 10_000;
/// Least `[control] swap_reference_max_age_secs`, in seconds.
pub const MIN_SWAP_REFERENCE_MAX_AGE_SECS: u32 = 30;
/// Largest `[control] swap_reference_max_age_secs`, in seconds.
pub const MAX_SWAP_REFERENCE_MAX_AGE_SECS: u32 = 3_600;

/// The keys of the control sections, with their section; one at the top level, or in a
/// table that keeps unknown keys, is refused.
const SECTION_KEYS: [(&str, &str); 10] = [
    ("auto_execute_commands", "control"),
    ("auto_accept_policies", "control"),
    ("swap_max_deviation_bps", "control"),
    ("swap_reference_max_age_secs", "control"),
    ("mode", "vetted_packages"),
    ("packages", "vetted_packages"),
    ("key_file", "controller"),
    ("agent_party", "controller"),
    ("network", "controller"),
    ("mode", "verify"),
];

/// The sections read in this strict pass.
const STRICT_SECTIONS: [&str; 4] = ["control", "vetted_packages", "controller", "verify"];

/// `[verify] mode`: how the verifier's newer checks act while the agent is not linked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyModeSetting {
    /// Each operation's default: the checks that are new to it are observed.
    #[default]
    Default,
    /// Observe the newer checks of every operation: log and count, sign as before.
    Observe,
    /// Enforce every check: refuse.
    Enforce,
}

impl VerifyModeSetting {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

/// `[verify]`: the verifier's settings; a linked agent enforces every check whatever they say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifySettings {
    #[serde(default)]
    pub mode: VerifyModeSetting,
}

/// The sections a section key belongs under, as `[a] or [b]`.
fn sections_of(key: &str) -> String {
    let names: Vec<String> =
        SECTION_KEYS.iter().filter(|(k, _)| *k == key).map(|(_, section)| format!("[{section}]")).collect();
    names.join(" or ")
}

/// The first section key, as `(key, where)`, in a table that keeps unknown keys; the strict
/// sections and `[markets.strategy]` refuse unknown keys themselves.
fn misplaced_section_key(table: &toml::Table) -> Option<(String, String)> {
    for (name, value) in table {
        if STRICT_SECTIONS.contains(&name.as_str()) {
            continue;
        }
        if let Some(found) = misplaced_in(value, name, None) {
            return Some(found);
        }
    }
    None
}

/// [`misplaced_section_key`] under the table or array of tables `value` at `path`; `entry`
/// names the array entry it is in, by its id when it has one.
fn misplaced_in(value: &toml::Value, path: &str, entry: Option<&str>) -> Option<(String, String)> {
    match value {
        toml::Value::Table(table) => misplaced_in_table(table, path, format!("[{path}]"), entry),
        toml::Value::Array(items) => items.iter().enumerate().find_map(|(i, item)| {
            let toml::Value::Table(table) = item else { return None };
            let own = ["market_id", "venue"].iter().find_map(|id| {
                table.get(*id).and_then(toml::Value::as_str).map(|v| format!("{id} = {v:?}"))
            });
            let own = own.unwrap_or_else(|| format!("entry {}", i.saturating_add(1)));
            let label = match entry {
                Some(outer) => format!("{outer}, {own}"),
                None => own,
            };
            misplaced_in_table(table, path, format!("[[{path}]]"), Some(&label))
        }),
        _ => None,
    }
}

fn misplaced_in_table(table: &toml::Table, path: &str, header: String, entry: Option<&str>) -> Option<(String, String)> {
    for (key, inner) in table {
        if SECTION_KEYS.iter().any(|(k, _)| k == key) {
            let at = match entry {
                Some(entry) => format!("{header} ({entry})"),
                None => header,
            };
            return Some((key.clone(), at));
        }
        if path == "markets" && key == "strategy" {
            continue;
        }
        if let Some(found) = misplaced_in(inner, &format!("{path}.{key}"), entry) {
            return Some(found);
        }
    }
    None
}

/// `[control]`: what a linked agent does by itself with pending controller work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlSettings {
    /// Execute pending direct commands without a prompt; otherwise they wait for
    /// `control execute`.
    #[serde(default)]
    pub auto_execute_commands: bool,
    /// Accept valid pending policies without a prompt; otherwise they wait for
    /// `control policy accept`.
    #[serde(default = "default_auto_accept_policies")]
    pub auto_accept_policies: bool,
    /// Largest gap between a swap's price and the agent's reference mid, in basis
    /// points; 0 turns the guard off.
    #[serde(default = "default_swap_max_deviation_bps")]
    pub swap_max_deviation_bps: u32,
    /// Oldest reference mid the swap price guard takes, in seconds (30 to 3600).
    #[serde(default = "default_swap_reference_max_age_secs")]
    pub swap_reference_max_age_secs: u32,
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            auto_execute_commands: false,
            auto_accept_policies: default_auto_accept_policies(),
            swap_max_deviation_bps: default_swap_max_deviation_bps(),
            swap_reference_max_age_secs: default_swap_reference_max_age_secs(),
        }
    }
}

impl ControlSettings {
    /// The settings as one log line.
    pub fn describe(&self) -> String {
        format!(
            "auto_execute_commands={} auto_accept_policies={} swap_max_deviation_bps={}{} swap_reference_max_age_secs={}",
            self.auto_execute_commands,
            self.auto_accept_policies,
            self.swap_max_deviation_bps,
            if self.swap_max_deviation_bps == 0 { " (the deviation guard is off)" } else { "" },
            self.swap_reference_max_age_secs
        )
    }
}

/// `[vetted_packages]`: the packages the agent expects its policy to allow.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VettedPackages {
    /// No section, or `mode = "all"`.
    #[default]
    All,
    /// `mode = "selected"` with 1 to 256 package ids.
    Selected(BTreeSet<agent_control::PackageId>),
}

impl VettedPackages {
    /// The setting as one log line.
    pub fn describe(&self) -> String {
        match self {
            Self::All => "all".to_string(),
            Self::Selected(ids) => format!("selected ({} packages)", ids.len()),
        }
    }
}

/// `[controller]`: this machine acting as an Ed25519 controller of one linked agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerSettings {
    /// The controller key file; [`load_controller_section`] resolves it against the
    /// directory of agent.toml when it is relative.
    pub key_file: PathBuf,
    /// The agent this key controls.
    pub agent_party: agent_control::PartyId,
    /// `devnet` or `mainnet`; `None` leaves the network to `CANTON_CHAIN`.
    pub network: Option<agent_control::Network>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControllerToml {
    key_file: String,
    agent_party: String,
    #[serde(default)]
    network: Option<String>,
}

/// `[controller]` of a parsed agent.toml, checked strictly; `key_file` is left as written.
fn controller_section(table: &toml::Table) -> Result<Option<ControllerSettings>> {
    let Some(value) = table.get("controller") else {
        return Ok(None);
    };
    let raw = value
        .clone()
        .try_into::<ControllerToml>()
        .map_err(|e| anyhow!("invalid [controller] section: {e}"))?;
    if raw.key_file.trim().is_empty() {
        bail!("invalid [controller] section: key_file is empty");
    }
    let agent_party = agent_control::PartyId::parse(raw.agent_party.trim()).map_err(|_| {
        anyhow!("invalid [controller] section: agent_party = {:?} is not a party id", raw.agent_party)
    })?;
    let network = match raw.network.as_deref().map(str::trim) {
        None => None,
        Some(text) => Some(agent_control::Network::parse(text).map_err(|_| {
            anyhow!("invalid [controller] section: network = {text:?} is neither \"devnet\" nor \"mainnet\"")
        })?),
    };
    Ok(Some(ControllerSettings { key_file: PathBuf::from(raw.key_file.trim()), agent_party, network }))
}

/// Reads only `[controller]` of `agent_toml`, strictly, and resolves a relative `key_file`
/// against the file's directory. `Ok(None)` when the file or the section is absent.
pub fn load_controller_section(agent_toml: &Path) -> Result<Option<ControllerSettings>> {
    if !agent_toml.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(agent_toml).with_context(|| format!("Failed to read {}", agent_toml.display()))?;
    let table: toml::Table =
        toml::from_str(&text).with_context(|| format!("{} is not valid TOML", agent_toml.display()))?;
    let Some(mut section) =
        controller_section(&table).with_context(|| format!("Failed to parse {}", agent_toml.display()))?
    else {
        return Ok(None);
    };
    if section.key_file.is_relative() {
        let base = agent_toml.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        section.key_file = base.join(&section.key_file);
    }
    Ok(Some(section))
}

/// The control sections of agent.toml, read in their own strict pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ControlSections {
    pub(crate) control: ControlSettings,
    pub(crate) vetted_packages: VettedPackages,
    pub(crate) controller: Option<ControllerSettings>,
    pub(crate) verify: VerifySettings,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VettedPackagesToml {
    mode: String,
    #[serde(default)]
    packages: Option<Vec<String>>,
}

/// Reads `[control]`, `[vetted_packages]`, `[controller]` and `[verify]`: an unknown key or a
/// bad value refuses. The other agent.toml keys are not read here.
pub(crate) fn parse_control_sections(toml_str: &str) -> Result<ControlSections> {
    let table: toml::Table = toml::from_str(toml_str).context("agent.toml is not valid TOML")?;
    // The other passes ignore an unknown top-level key, so a section key there would be lost.
    if let Some((key, _)) = SECTION_KEYS.iter().find(|(key, _)| table.contains_key(*key)) {
        bail!("{key} belongs under {}, not at the top level of agent.toml", sections_of(key));
    }
    // A key below a table header joins that table, whose unknown keys are ignored too.
    if let Some((key, at)) = misplaced_section_key(&table) {
        bail!(
            "{key} belongs under {}, not under {at} of agent.toml: a key joins the table whose \
             header is above it",
            sections_of(&key)
        );
    }
    let control = match table.get("control") {
        None => ControlSettings::default(),
        Some(value) => value
            .clone()
            .try_into::<ControlSettings>()
            .map_err(|e| anyhow!("invalid [control] section: {e}"))?,
    };
    if control.swap_max_deviation_bps > MAX_SWAP_DEVIATION_BPS {
        bail!(
            "invalid [control] section: swap_max_deviation_bps = {} is above {MAX_SWAP_DEVIATION_BPS} \
             (100 %); 0 turns the guard off",
            control.swap_max_deviation_bps
        );
    }
    let age = control.swap_reference_max_age_secs;
    if !(MIN_SWAP_REFERENCE_MAX_AGE_SECS..=MAX_SWAP_REFERENCE_MAX_AGE_SECS).contains(&age) {
        bail!(
            "invalid [control] section: swap_reference_max_age_secs = {age} is not between \
             {MIN_SWAP_REFERENCE_MAX_AGE_SECS} and {MAX_SWAP_REFERENCE_MAX_AGE_SECS} seconds"
        );
    }
    let vetted_packages = match table.get("vetted_packages") {
        None => VettedPackages::All,
        Some(value) => {
            let raw = value
                .clone()
                .try_into::<VettedPackagesToml>()
                .map_err(|e| anyhow!("invalid [vetted_packages] section: {e}"))?;
            vetted_packages_of(raw).context("invalid [vetted_packages] section")?
        }
    };
    let controller = controller_section(&table)?;
    let verify = match table.get("verify") {
        None => VerifySettings::default(),
        Some(value) => {
            value.clone().try_into::<VerifySettings>().map_err(|e| anyhow!("invalid [verify] section: {e}"))?
        }
    };
    Ok(ControlSections { control, vetted_packages, controller, verify })
}

fn vetted_packages_of(raw: VettedPackagesToml) -> Result<VettedPackages> {
    match (raw.mode.as_str(), raw.packages) {
        ("all", None) => Ok(VettedPackages::All),
        ("all", Some(_)) => bail!("mode = \"all\" takes no packages list"),
        ("selected", None) => bail!("mode = \"selected\" needs a packages list"),
        ("selected", Some(list)) => {
            let max = agent_control::MAX_PACKAGES;
            if list.is_empty() || list.len() > max {
                bail!("mode = \"selected\" takes 1 to {max} package ids, not {}", list.len());
            }
            let mut ids = BTreeSet::new();
            for (i, text) in list.iter().enumerate() {
                let id = agent_control::PackageId::parse(text).map_err(|_| {
                    anyhow!("packages[{i}] = {text:?} is not a package id (64 lowercase hex characters)")
                })?;
                if !ids.insert(id) {
                    bail!("packages[{i}] = {text:?} is listed twice");
                }
            }
            Ok(VettedPackages::Selected(ids))
        }
        (other, _) => bail!("mode = {other:?} is neither \"all\" nor \"selected\""),
    }
}

// ============================================================================
// Defaults
// ============================================================================

fn default_auto_accept_policies() -> bool {
    true
}

fn default_swap_max_deviation_bps() -> u32 {
    100
}

fn default_swap_reference_max_age_secs() -> u32 {
    300
}

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

fn default_balance_stale_after_secs() -> u64 {
    120
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
        let mut config = BaseConfig::test_minimal().unwrap();
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

    fn listed(id: &str, kind: &str, symbol: &str, registry: Option<&str>) -> Instrument {
        Instrument {
            instrument_id: id.into(),
            instrument_type: kind.into(),
            symbol: symbol.into(),
            registry: registry.map(Into::into),
            ..Default::default()
        }
    }

    // An unknown or registry-less instrument used to resolve to an empty admin, which verification skips
    #[test]
    fn try_resolve_instrument_refuses_what_the_registry_cannot_verify() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.dso_party = "dso::1220aa".into();
        config.populate_instruments_from_rpc(vec![
            listed(
                "ACME",
                "fiat",
                "0a1b2c3d-1111-4222-8333-444455556666",
                Some("issuer-reg::1220cc"),
            ),
            listed("CC", "token", "CC", Some("dso::1220aa")),
            listed("NOREG", "fiat", "NOREG", None),
            listed("BLANK", "fiat", "BLANK", Some("  ")),
        ]);
        assert_eq!(
            config.try_resolve_instrument("ACME").unwrap(),
            (
                "0a1b2c3d-1111-4222-8333-444455556666".to_string(),
                "issuer-reg::1220cc".to_string()
            )
        );
        assert_eq!(
            config.try_resolve_instrument("CC").unwrap(),
            ("Amulet".to_string(), "dso::1220aa".to_string())
        );
        for missing in ["UNLISTED", "NOREG", "BLANK", ""] {
            let err = config
                .try_resolve_instrument(missing)
                .unwrap_err()
                .to_string();
            assert!(
                err.starts_with(UNRESOLVED_INSTRUMENT) && err.contains(&format!("'{missing}'")),
                "{err}"
            );
            assert!(err.contains("among the 4 loaded instruments"), "{err}");
        }
        assert_eq!(
            config.resolve_instrument("UNLISTED"),
            ("UNLISTED".to_string(), String::new()),
            "the display resolver is unchanged"
        );
        assert!(
            BaseConfig::test_minimal()
                .unwrap()
                .try_resolve_instrument("CC")
                .is_err()
        );
    }

    // Canton Coin's admin is the DSO party even when the listing omits its registry
    #[test]
    fn canton_coin_falls_back_to_the_dso_party() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.dso_party = "dso::1220aa".into();
        config.populate_instruments_from_rpc(vec![listed("CC", "token", "CC", None)]);
        assert_eq!(config.cc_token_id.as_deref(), Some("CC"));
        assert_eq!(
            config.resolve_instrument("CC"),
            ("Amulet".to_string(), String::new())
        );
        assert_eq!(
            config.try_resolve_instrument("CC").unwrap(),
            ("Amulet".to_string(), "dso::1220aa".to_string())
        );
        config.dso_party.clear();
        assert!(
            config
                .try_resolve_instrument("CC")
                .unwrap_err()
                .to_string()
                .starts_with(UNRESOLVED_INSTRUMENT)
        );
    }

    // The daemon used to run on an empty or CC-less registry
    #[test]
    fn validate_instrument_registry_needs_instruments_registries_and_canton_coin() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.dso_party = "dso::1220aa".into();
        let err = |c: &BaseConfig| c.validate_instrument_registry().unwrap_err().to_string();
        assert_eq!(err(&config), "orderbook-rpc listed no instruments");
        config.populate_instruments_from_rpc(vec![listed("TKN", "fiat", "TKN", None)]);
        assert_eq!(
            err(&config),
            "none of the 1 listed instruments carries a registry party"
        );
        config.populate_instruments_from_rpc(vec![listed("TKN", "fiat", "TKN", Some("  "))]);
        assert_eq!(
            err(&config),
            "none of the 1 listed instruments carries a registry party"
        );
        config.populate_instruments_from_rpc(vec![listed(
            "TKN",
            "fiat",
            "TKN",
            Some("tkn-reg::1220bb"),
        )]);
        assert!(
            err(&config).starts_with("no Canton Coin instrument"),
            "{}",
            err(&config)
        );
        config.populate_instruments_from_rpc(vec![
            listed("TKN", "fiat", "TKN", Some("tkn-reg::1220bb")),
            listed("CC", "token", "CC", None),
        ]);
        config.validate_instrument_registry().unwrap();
        config.dso_party.clear();
        assert!(
            err(&config).starts_with(UNRESOLVED_INSTRUMENT),
            "{}",
            err(&config)
        );
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

    /// The side flags default to open and are pair-level only: a venue overlay
    /// carries them through unchanged and can never re-open a closed side.
    #[test]
    fn test_side_flags_default_open_and_survive_venue_overlay() {
        let open: RfqMarketConfig = serde_json::from_str(
            r#"{"min_quantity":"50","max_quantity":"10000"}"#,
        )
        .unwrap();
        assert!(!open.disable_offer_side, "absent key must parse as open");
        assert!(!open.disable_bid_side, "absent key must parse as open");

        let bid_only: RfqMarketConfig = serde_json::from_str(
            r#"{"min_quantity":"50","max_quantity":"10000","disable_offer_side":true}"#,
        )
        .unwrap();
        // An overlay that rewrites every field it CAN must still leave the
        // closed side closed.
        let overrides: Vec<VenueOverride> = toml::from_str::<AgentToml>(
            r#"
[[venue_overrides]]
venue = "walley"
[venue_overrides.rfq]
enabled = true
bid_spread_percent = 4.0
offer_spread_percent = 4.0
"#,
        )
        .unwrap()
        .venue_overrides;
        let r = resolve_rfq_config(&bid_only, &overrides, "USD8-CC", Some("walley"), None);
        assert!(matches!(r, std::borrow::Cow::Owned(_)), "overlay must apply");
        assert_eq!(r.offer_spread_percent, 4.0, "overlay still rewrites spreads");
        assert!(r.disable_offer_side, "a venue must not re-open a closed side");
        assert!(!r.disable_bid_side, "the open side stays open");
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
    fn a_party_is_matched_against_its_own_public_key() {
        // Real devnet pair: the namespace is SHA256(0x0000000C || public key).
        let public_key: [u8; 32] =
            hex::decode("9895938290240e991e06356233ff0694fb3cb90e89443fd95f54048e64c2621f")
                .unwrap()
                .try_into()
                .unwrap();
        let party = "99a24d3a73eb9a1c::1220dda5272ceb983cf00699447d878078c1df7dcdf1c64955e444b7483df3e8384b";
        assert_eq!(party_matches_public_key(party, &public_key), Some(true));

        let other = "bridge-1::12200c73731c0c8d94327e71e966ca6ff356732c3298797e5eb73eece523582c46dd";
        assert_eq!(party_matches_public_key(other, &public_key), Some(false));
    }

    #[test]
    fn a_party_without_a_derivable_namespace_is_not_judged() {
        let key = [0u8; 32];
        assert_eq!(party_matches_public_key("lp::1220bb", &key), None);
        assert_eq!(party_matches_public_key("no-namespace", &key), None);
        assert_eq!(party_matches_public_key("lp::abcd", &key), None);
    }

    #[test]
    fn pinned_platform_parties_must_be_party_ids() {
        assert_eq!(pinned_party("PARTY_FEE", None).unwrap(), None);
        assert_eq!(
            pinned_party("PARTY_FEE", Some("fee::1220ab".into())).unwrap().as_deref(),
            Some("fee::1220ab")
        );
        for bad in ["fee", "fee ::1220ab", "::"] {
            let err = pinned_party("PARTY_FEE", Some(bad.into()));
            assert!(bad == "::" || err.is_err(), "{bad}");
        }
        let mut config = BaseConfig::test_minimal().unwrap();
        assert!(config.platform_parties().is_empty());
        config.party_prepaid_traffic = Some("traffic::1220cd".into());
        assert_eq!(config.platform_parties(), vec!["traffic::1220cd"]);
    }

    #[test]
    fn require_party_matches_key_accepts_only_a_match() {
        let public_key: [u8; 32] =
            hex::decode("9895938290240e991e06356233ff0694fb3cb90e89443fd95f54048e64c2621f")
                .unwrap()
                .try_into()
                .unwrap();
        let party = "99a24d3a73eb9a1c::1220dda5272ceb983cf00699447d878078c1df7dcdf1c64955e444b7483df3e8384b";
        assert!(require_party_matches_key(party, &public_key).is_ok());
        assert_eq!(party.split("::").nth(1), Some(key_namespace(&public_key).as_str()));
        let other = "bridge-1::12200c73731c0c8d94327e71e966ca6ff356732c3298797e5eb73eece523582c46dd";
        let err = require_party_matches_key(other, &public_key).unwrap_err().to_string();
        assert!(err.contains("does not belong to this key"), "{err}");
        for unjudged in ["lp::1220bb", "no-namespace", "lp::abcd", ""] {
            let err = require_party_matches_key(unjudged, &public_key).unwrap_err().to_string();
            assert!(err.contains("no namespace"), "{unjudged}: {err}");
        }
    }

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
        let (k2_b58, _) = crate::sign::generate_keypair().unwrap();
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

        // P: knobs that size pools or feed timers are validated, never defaulted
        unset("RFQ_V2_ENABLED");
        for (k, v) in [
            ("SETTLEMENT_THREAD_COUNT", "0"),
            ("SETTLEMENT_THREAD_COUNT", "1025"),
            ("SETTLEMENT_THREAD_COUNT", "many"),
            ("MERGE_POLL_INTERVAL_SEC", "0"),
            ("AGENT_FEE_RESERVE_CC", "NaN"),
            ("LIQUIDITY_MARGIN", "1,1"),
            ("DEPLETION_COEFF_MIN_HOURS", "inf"),
        ] {
            set(k, v);
            let err = format!("{:#}", BaseConfig::assemble(toml::from_str::<AgentToml>("").unwrap(), ConfigOverrides::default()).unwrap_err());
            assert!(err.contains(k), "{k}={v}: {err}");
            unset(k);
        }
        set("SETTLEMENT_THREAD_COUNT", " 1024 ");
        set("MERGE_POLL_INTERVAL_SEC", "");
        let cfg = BaseConfig::assemble(toml::from_str::<AgentToml>("").unwrap(), ConfigOverrides::default()).unwrap();
        assert_eq!((cfg.settlement_thread_count, cfg.merge_poll_interval_sec), (1024, 600));
        unset("SETTLEMENT_THREAD_COUNT");
        unset("MERGE_POLL_INTERVAL_SEC");

        // Q: agent.toml timeouts and an overlay max_quantity of zero are rejected
        let agent: AgentToml = toml::from_str("token_ttl_secs = 59").unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("token_ttl_secs"), "got: {err}");
        let agent: AgentToml = toml::from_str(
            r#"
[[venue_overrides]]
venue = "walley"
[venue_overrides.rfq]
max_quantity = "0"
"#,
        )
        .unwrap();
        let err = BaseConfig::assemble(agent, ConfigOverrides::default()).unwrap_err().to_string();
        assert!(err.contains("max_quantity '0' is not a positive number"), "got: {err}");

        // cleanup
        for k in [
            "RFQ_V2_ENABLED",
            "RFQ_V2_ONLY",
            "TICKET_THRESHOLD_USD",
            "TICKET_BATCH_SIZE",
            "ATOMIC_QUOTE_PRIVATE_KEY",
            "SETTLEMENT_THREAD_COUNT",
            "MERGE_POLL_INTERVAL_SEC",
            "AGENT_FEE_RESERVE_CC",
            "LIQUIDITY_MARGIN",
            "DEPLETION_COEFF_MIN_HOURS",
        ] {
            unset(k);
        }
    }

    #[test]
    fn validate_ranges_rejects_values_that_feed_pools_and_timers() {
        fn with_v2(c: &mut BaseConfig) {
            c.liquidity_provider = Some(LiquidityProviderConfig {
                name: "LP test".to_string(),
                max_concurrent_rfqs: 1,
                default_quote_valid_secs: 30,
                min_notional_usd: 0.0,
                rfq_v2: Some(RfqV2Config::default()),
            });
        }
        fn v2(c: &mut BaseConfig) -> &mut RfqV2Config {
            c.liquidity_provider.as_mut().unwrap().rfq_v2.as_mut().unwrap()
        }
        let cases: Vec<(&str, fn(&mut BaseConfig))> = vec![
            ("SETTLEMENT_THREAD_COUNT", |c| c.settlement_thread_count = 0),
            ("SETTLEMENT_THREAD_COUNT", |c| c.settlement_thread_count = 1025),
            ("MERGE_POLL_INTERVAL_SEC", |c| c.merge_poll_interval_sec = 0),
            ("token_ttl_secs", |c| c.token_ttl_secs = 59),
            ("token_ttl_secs", |c| c.token_ttl_secs = 86_401),
            ("connection_timeout_secs", |c| c.connection_timeout_secs = 0),
            ("connection_timeout_secs", |c| c.connection_timeout_secs = 301),
            ("request_timeout_secs", |c| c.request_timeout_secs = 0),
            ("request_timeout_secs", |c| c.request_timeout_secs = 3601),
            ("canton_op_timeout_secs", |c| c.canton_op_timeout_secs = 0),
            ("canton_op_timeout_secs", |c| c.canton_op_timeout_secs = 86_401),
            ("AGENT_FEE_RESERVE_CC", |c| c.fee_reserve_cc = f64::NAN),
            ("LIQUIDITY_MARGIN", |c| c.liquidity_margin = f64::INFINITY),
            ("FLOW_EMA_WINDOW_HOURS", |c| c.flow_ema_window_hours = f64::NAN),
            ("DEPLETION_COEFF_MAX_HOURS", |c| c.depletion_max_hours = f64::NEG_INFINITY),
            ("DEPLETION_COEFF_MIN_HOURS", |c| c.depletion_min_hours = f64::NAN),
            ("AGENT_SETTLEMENT_TRAFFIC_CC", |c| c.settlement_traffic_cc = f64::NAN),
            ("AGENT_SETTLEMENT_TRAFFIC_CC", |c| c.settlement_traffic_cc = -0.1),
            ("ticket_batch_size", |c| { with_v2(c); v2(c).ticket_batch_size = 0 }),
            ("ticket_batch_size", |c| { with_v2(c); v2(c).ticket_batch_size = 1001 }),
            ("atomic_quote_valid_secs", |c| { with_v2(c); v2(c).atomic_quote_valid_secs = 0 }),
            ("atomic_quote_valid_secs", |c| { with_v2(c); v2(c).atomic_quote_valid_secs = 3601 }),
            ("settle_grace_secs", |c| { with_v2(c); v2(c).settle_grace_secs = 3601 }),
            ("updates_poll_interval_secs", |c| { with_v2(c); v2(c).updates_poll_interval_secs = 0 }),
            ("split_poll_interval_secs", |c| { with_v2(c); v2(c).split_poll_interval_secs = 0 }),
        ];
        for (name, mutate) in &cases {
            let mut config = BaseConfig::test_minimal().unwrap();
            mutate(&mut config);
            let err = config.validate_v2().unwrap_err().to_string();
            assert!(err.contains(name), "{name}: {err}");
        }

        // The bounds themselves are accepted
        let mut config = BaseConfig::test_minimal().unwrap();
        with_v2(&mut config);
        config.settlement_thread_count = 1024;
        config.token_ttl_secs = 86_400;
        config.connection_timeout_secs = 300;
        config.request_timeout_secs = 3600;
        config.canton_op_timeout_secs = 86_400;
        let lp = v2(&mut config);
        lp.ticket_batch_size = 1000;
        lp.atomic_quote_valid_secs = 3600;
        lp.settle_grace_secs = 3600;
        config.validate_ranges().unwrap();
    }

    #[test]
    fn every_rfq_market_needs_a_positive_max_quantity() {
        let market = |max: &str| -> MarketConfig {
            toml::from_str(&format!(
                "market_id = \"CC-USDCx\"\n[rfq]\nmin_quantity = \"1\"\nmax_quantity = \"{max}\"\n"
            ))
            .unwrap()
        };
        for bad in ["", " 1", "abc", "0", "-1", "inf", "NaN"] {
            let mut config = BaseConfig::test_minimal().unwrap();
            config.markets = vec![market(bad)];
            let err = config.validate_ranges().unwrap_err().to_string();
            assert!(err.contains("max_quantity"), "{bad:?}: {err}");
        }
        let mut config = BaseConfig::test_minimal().unwrap();
        config.markets = vec![market("0.1")];
        config.validate_ranges().unwrap();
    }

    #[test]
    fn env_values_parse_strictly_with_blank_as_unset() {
        assert_eq!(parse_env_value("N", None, 25usize).unwrap(), 25);
        assert_eq!(parse_env_value("N", Some("  "), 25usize).unwrap(), 25);
        assert_eq!(parse_env_value("N", Some(" 7 "), 25usize).unwrap(), 7);
        assert!(parse_env_value("N", Some("1.5"), 25usize).unwrap_err().to_string().starts_with("N='1.5'"));
        assert!(parse_env_value("N", Some("-3"), 25usize).is_err());
        assert!(parse_env_value("N", Some("NaN"), 1.0f64).unwrap().is_nan(), "finiteness is checked by validate_ranges");
    }

    #[test]
    fn keys_of_the_wrong_length_are_errors() {
        let short = bs58::encode([1u8; 31]).into_string();
        let err = decode_public_key(&short).unwrap_err().to_string();
        assert_eq!(err, "Public key must be exactly 32 bytes, got 31");
        assert!(decode_public_key(&bs58::encode([1u8; 33]).into_string()).is_err());
        assert_eq!(decode_public_key(&bs58::encode([5u8; 32]).into_string()).unwrap(), [5u8; 32]);
        let err = decode_private_key(&short).unwrap_err().to_string();
        assert_eq!(err, "Private key too short: expected at least 32 bytes, got 31");
        let mut keypair = [9u8; 64];
        keypair[..32].copy_from_slice(&[4u8; 32]);
        assert_eq!(decode_private_key(&bs58::encode(keypair).into_string()).unwrap(), [4u8; 32]);
    }

    #[test]
    fn test_quote_key_from_short_scalar_hex() {
        let short = "01".repeat(24);
        let key = AtomicQuoteKey::from_scalar_hex(&short).unwrap();
        assert_eq!(key.scalar_hex().unwrap().len(), 64);
        assert!(key.scalar_hex().unwrap().starts_with(&"0".repeat(16)));
        assert!(key.scalar_hex().unwrap().ends_with(&short));
        assert!(AtomicQuoteKey::from_scalar_hex(&"01".repeat(10)).is_err());
    }

    // A quote key that cannot be opened is an error, not a panic
    #[test]
    fn a_quote_key_that_cannot_be_opened_is_an_error() {
        let key = AtomicQuoteKey { pub_spki_hex: String::new(), scalar: Secret::corrupt_for_tests() };
        assert!(key.scalar().is_err());
        assert_eq!(key.scalar_hex().unwrap_err(), crate::secret::SecretError::Corrupt);
    }

    #[test]
    fn test_quote_key_from_scalar_hex() {
        let kf = atomic_quote::gen_keypair().unwrap();
        let key = AtomicQuoteKey::from_scalar_hex(&kf.priv_scalar_hex).unwrap();
        assert_eq!(key.pub_spki_hex, kf.pub_spki_hex);
        assert_eq!(*key.scalar_hex().unwrap(), kf.priv_scalar_hex.to_lowercase());
        assert_eq!(hex::encode(key.scalar().unwrap().as_slice()), kf.priv_scalar_hex.to_lowercase());
        assert_eq!(format!("{key:?}"), format!("AtomicQuoteKey {{ pub_spki_hex: {:?}, .. }}", kf.pub_spki_hex));
        assert!(AtomicQuoteKey::from_scalar_hex("zz").is_err());
    }

    /// Non-env mirror: the same rfq_v2_only requirements enforced by
    /// `validate_v2` for `for_party` embedders.
    #[test]
    fn test_rfq_v2_only_validate_v2_mirror() {
        let mut config = BaseConfig::test_minimal().unwrap();
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

    #[test]
    fn market_quote_validity_may_only_shorten_the_lp_window() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.liquidity_provider = Some(LiquidityProviderConfig {
            name: "LP test".to_string(),
            max_concurrent_rfqs: default_max_concurrent_rfqs(),
            default_quote_valid_secs: default_quote_valid_secs(),
            min_notional_usd: 0.0,
            rfq_v2: Some(RfqV2Config { enabled: true, ..RfqV2Config::default() }),
        });
        let kf = atomic_quote::gen_keypair().unwrap();
        config.set_atomic_quote_scalar(&kf.priv_scalar_hex).unwrap();
        let market_with = |secs: Option<u64>| -> MarketConfig {
            let line = secs.map(|s| format!("atomic_quote_valid_secs = {s}")).unwrap_or_default();
            toml::from_str(&format!(
                "market_id = \"TKN-CC\"\n\n[rfq]\nmin_quantity = \"5\"\nmax_quantity = \"1000\"\n\n\
                 [rfq.v2]\nenabled = true\n{line}\n"
            ))
            .unwrap()
        };

        for (secs, ok) in [(None, true), (Some(60), true), (Some(120), true), (Some(30), false), (Some(59), false), (Some(121), false)] {
            let market = market_with(secs);
            assert_eq!(market.rfq.as_ref().unwrap().v2.as_ref().unwrap().atomic_quote_valid_secs, secs);
            config.markets = vec![market];
            config.recompute_deadline_windows();
            match config.validate_v2() {
                Ok(()) => assert!(ok, "{secs:?} accepted"),
                Err(e) => {
                    assert!(!ok, "{secs:?} rejected: {e}");
                    assert!(e.to_string().contains("atomic_quote_valid_secs"), "got: {e}");
                }
            }
        }

        // Without an LP-level config the cap is the largest accepted window.
        let v2m: RfqV2MarketConfig =
            toml::from_str("enabled = true\natomic_quote_valid_secs = 3600\n").unwrap();
        check_market_quote_valid_secs("X-Y", &v2m, None).unwrap();
    }
}

#[cfg(test)]
mod strategy_table_tests {
    use super::*;

    // The first pass ignores strategy tables, so MarketConfig stays as it was
    #[test]
    fn agent_toml_still_parses_a_file_with_strategy_tables() {
        let toml = r#"
            [[markets]]
            market_id = "HTEST-USDC"
            [markets.strategy]
            kind = "rebalancer"
            target_share = 0.25
        "#;
        let agent: AgentToml = toml::from_str(toml).unwrap();
        assert_eq!(agent.markets.len(), 1);
        assert!(agent.markets[0].bid_levels.is_empty());
        let strategies = crate::strategy::config::parse_strategies(toml).unwrap();
        assert_eq!(strategies.get("HTEST-USDC").map(StrategyConfig::kind), Some("rebalancer"));
    }

    // Copies of the two examples in the README's Strategies section
    const README_MAKER: &str = r#"
role = "trader"
auto_settle = true
poll_interval_secs = 5

[[markets]]
market_id = "CC-USDC"
enabled = true

[markets.strategy]
kind = "market_maker"
levels = 3
half_spread_bps = 15
level_step_bps = 15
level_usd = 60
max_inventory_usd = 500
skew_bps = 20
"#;

    const README_TAKER: &str = r#"
role = "trader"
auto_settle = true
poll_interval_secs = 5

[[markets]]
market_id = "CC-USDC"
enabled = true

[markets.strategy]
kind = "twap"
side = "buy"
total_quantity = "2000"
start_at = "2026-12-01T10:00:00Z"
duration_secs = 3600
slice_secs = 240
max_slip_bps = 15
"#;

    #[test]
    fn the_readme_strategy_examples_parse_and_validate() {
        for (toml, kind) in [(README_MAKER, "market_maker"), (README_TAKER, "twap")] {
            let agent: AgentToml = toml::from_str(toml).unwrap();
            let mut config = BaseConfig::test_minimal().unwrap();
            config.markets = agent.markets;
            config.liquidity_provider = agent.liquidity_provider;
            config.strategies = crate::strategy::config::parse_strategies(toml).unwrap();
            config.validate_ranges().unwrap();
            assert_eq!(config.strategy_for("CC-USDC").kind(), kind);
        }
        let StrategyConfig::MarketMaker(mm) = parse_one(README_MAKER) else { panic!() };
        assert_eq!((mm.levels, mm.half_spread_bps, mm.level_usd, mm.skew_bps), (3, 15.0, 60.0, 20.0));
        let StrategyConfig::Twap(tw) = parse_one(README_TAKER) else { panic!() };
        assert_eq!((tw.total_quantity.as_deref(), tw.duration_secs, tw.passive_secs), (Some("2000"), 3600, 60));
    }

    fn parse_one(toml: &str) -> StrategyConfig {
        crate::strategy::config::parse_strategies(toml).unwrap().remove("CC-USDC").unwrap()
    }

    #[test]
    fn markets_without_a_table_run_the_grid() {
        let config = BaseConfig::test_minimal().unwrap();
        assert!(config.strategies.is_empty());
        assert!(config.strategy_for("CC-USDC").is_grid());
        let key = bs58::encode([7u8; 32]).into_string();
        let ledger = bs58::encode([9u8; 32]).into_string();
        let config = BaseConfig::for_party("p::1220", &key, "http://x", "s", "o", "d", "n", &ledger).unwrap();
        assert!(config.strategies.is_empty());
        assert!(config.strategy_for("CC-USDC").is_grid());
    }
}

#[cfg(test)]
mod control_token_tests {
    use super::*;
    use base64::Engine;

    fn claims(token: &str) -> serde_json::Value {
        let body = token.split('.').nth(1).unwrap();
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body).unwrap();
        serde_json::from_slice(&raw).unwrap()
    }

    #[test]
    fn control_tokens_name_the_control_services_of_canton_chain_and_others_name_none() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.private_key = Secret::seal(&mut [0xd3u8; 32]).unwrap();
        for (chain, aud) in [("devnet", "silvana.agent.control.devnet"), ("mainnet", "silvana.agent.control.mainnet")] {
            config.canton_chain = Some(chain.into());
            let (token, _) = config.control_jwt().unwrap();
            assert_eq!(claims(&token)["aud"].as_str(), Some(aud));
        }
        assert!(claims(&config.jwt().unwrap()).get("aud").is_none(), "other tokens are unchanged");
        for chain in [None, Some("testnet".to_string())] {
            config.canton_chain = chain;
            let err = config.control_jwt().unwrap_err().to_string();
            assert!(err.contains("CANTON_CHAIN"), "{err}");
        }
    }
}

#[cfg(test)]
mod control_section_tests {
    use super::*;

    const ID_A: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const ID_B: &str = "ab00000000000000000000000000000000000000000000000000000000000002";

    fn sections(toml: &str) -> ControlSections {
        parse_control_sections(toml).unwrap()
    }

    fn refusal(toml: &str) -> String {
        format!("{:#}", parse_control_sections(toml).unwrap_err())
    }

    #[test]
    fn absent_sections_take_the_defaults() {
        let parsed = sections("auto_settle = true\n[[markets]]\nmarket_id = \"CC-USDC\"\n");
        assert_eq!(parsed, ControlSections::default());
        let control = parsed.control;
        assert!(!control.auto_execute_commands && control.auto_accept_policies);
        assert_eq!(control.swap_max_deviation_bps, 100);
        assert_eq!(parsed.vetted_packages, VettedPackages::All);
        // An empty section is the same as none.
        assert_eq!(sections("[control]\n[vetted_packages]\nmode = \"all\"\n"), ControlSections::default());
        let config = BaseConfig::test_minimal().unwrap();
        assert_eq!((config.control, config.vetted_packages), (ControlSettings::default(), VettedPackages::All));
    }

    #[test]
    fn control_settings_are_read_and_bounded() {
        let parsed = sections(
            "[control]\nauto_execute_commands = true\nauto_accept_policies = false\nswap_max_deviation_bps = 0\n",
        );
        assert_eq!(
            parsed.control,
            ControlSettings {
                auto_execute_commands: true,
                auto_accept_policies: false,
                swap_max_deviation_bps: 0,
                swap_reference_max_age_secs: 300,
            }
        );
        assert!(parsed.control.describe().contains("guard is off"), "{}", parsed.control.describe());
        assert_eq!(sections("[control]\nswap_max_deviation_bps = 10000\n").control.swap_max_deviation_bps, 10_000);
        for bad in ["10001", "-1", "1.5", "\"100\""] {
            let err = refusal(&format!("[control]\nswap_max_deviation_bps = {bad}\n"));
            assert!(err.contains("[control]"), "{bad}: {err}");
        }
        let err = refusal("[control]\nauto_execute_commands = \"yes\"\n");
        assert!(err.contains("[control]"), "{err}");
    }

    #[test]
    fn the_swap_reference_age_defaults_to_300_and_takes_30_to_3600() {
        assert_eq!(ControlSettings::default().swap_reference_max_age_secs, 300);
        let described = ControlSettings::default().describe();
        assert!(described.ends_with(" swap_reference_max_age_secs=300"), "{described}");
        for ok in [30, 300, 3600] {
            let control = sections(&format!("[control]\nswap_reference_max_age_secs = {ok}\n")).control;
            assert_eq!(control.swap_reference_max_age_secs, ok);
            assert!(control.describe().ends_with(&format!(" swap_reference_max_age_secs={ok}")), "{ok}");
        }
        for bad in ["0", "29", "3601"] {
            let err = refusal(&format!("[control]\nswap_reference_max_age_secs = {bad}\n"));
            assert!(err.contains("[control]") && err.contains(&format!("swap_reference_max_age_secs = {bad}")), "{err}");
            assert!(err.contains("30") && err.contains("3600"), "the message names the range: {err}");
        }
        for bad in ["-1", "1.5", "\"300\""] {
            let err = refusal(&format!("[control]\nswap_reference_max_age_secs = {bad}\n"));
            assert!(err.contains("[control]"), "{bad}: {err}");
        }
    }

    #[test]
    fn unknown_keys_are_refused_with_the_section_and_key() {
        let err = refusal("[control]\nauto_execute = true\n");
        assert!(err.contains("[control]") && err.contains("auto_execute"), "{err}");
        assert!(err.contains("auto_execute_commands"), "the message lists the known keys: {err}");
        let err = refusal("[vetted_packages]\nmode = \"all\"\nlist = []\n");
        assert!(err.contains("[vetted_packages]") && err.contains("list"), "{err}");
        // The sections must be tables.
        assert!(refusal("control = 1\n").contains("[control]"));
        assert!(refusal("vetted_packages = \"all\"\n").contains("[vetted_packages]"));
    }

    #[test]
    fn a_section_key_at_the_top_level_is_refused_with_its_section() {
        let cases = [
            ("auto_execute_commands = true", "auto_execute_commands belongs under [control]"),
            ("auto_accept_policies = false", "auto_accept_policies belongs under [control]"),
            ("swap_max_deviation_bps = 50", "swap_max_deviation_bps belongs under [control]"),
            ("swap_reference_max_age_secs = 600", "swap_reference_max_age_secs belongs under [control]"),
            ("mode = \"all\"", "mode belongs under [vetted_packages]"),
            ("packages = []", "packages belongs under [vetted_packages]"),
            ("key_file = \"k\"", "key_file belongs under [controller]"),
            ("agent_party = \"p\"", "agent_party belongs under [controller]"),
            ("network = \"devnet\"", "network belongs under [controller]"),
        ];
        for (line, expected) in cases {
            let err = refusal(&format!("auto_settle = true\n{line}\n[[markets]]\nmarket_id = \"CC-USDC\"\n"));
            assert!(err.contains(expected), "{line}: {err}");
        }
        // The same keys under their sections load.
        let parsed = sections(&format!(
            "auto_settle = true\n[control]\nauto_execute_commands = true\nauto_accept_policies = false\n\
             swap_max_deviation_bps = 50\nswap_reference_max_age_secs = 600\n\
             [vetted_packages]\nmode = \"selected\"\npackages = [\"{ID_A}\"]\n\
             [controller]\nkey_file = \"k\"\nagent_party = \"{PARTY}\"\nnetwork = \"devnet\"\n"
        ));
        let control = parsed.control;
        assert!(control.auto_execute_commands && !control.auto_accept_policies, "{control:?}");
        assert_eq!((control.swap_max_deviation_bps, control.swap_reference_max_age_secs), (50, 600));
        assert_eq!(parsed.vetted_packages.describe(), "selected (1 packages)");
        assert_eq!(parsed.controller.and_then(|c| c.network), Some(agent_control::Network::Devnet));
        // The daemon's loaders refuse such a file too.
        let path = std::env::temp_dir().join(format!("agent-misplaced-key-{}.toml", std::process::id()));
        std::fs::write(&path, "auto_settle = true\nauto_execute_commands = true\n").unwrap();
        let strict = BaseConfig::load(&path).map(|_| ()).unwrap_err();
        let lenient = BaseConfig::load_or_defaults(&path).map(|_| ()).unwrap_err();
        let _ = std::fs::remove_file(&path);
        for err in [strict, lenient] {
            let text = format!("{err:#}");
            assert!(text.contains("auto_execute_commands belongs under [control]"), "{text}");
        }
    }

    #[test]
    fn the_verify_section_takes_default_observe_or_enforce() {
        assert_eq!(sections("auto_settle = true\n").verify.mode, VerifyModeSetting::Default);
        assert_eq!(sections("[verify]\n").verify, VerifySettings::default());
        for (text, mode) in [
            ("default", VerifyModeSetting::Default),
            ("observe", VerifyModeSetting::Observe),
            ("enforce", VerifyModeSetting::Enforce),
        ] {
            let parsed = sections(&format!("[verify]\nmode = \"{text}\"\n"));
            assert_eq!(parsed.verify.mode, mode);
            assert_eq!(mode.as_str(), text);
        }
        for body in ["mode = \"strict\"", "mode = 1", "mode = \"Observe\"", "level = \"observe\""] {
            let err = refusal(&format!("[verify]\n{body}\n"));
            assert!(err.contains("[verify]"), "{body}: {err}");
        }
        assert!(refusal("verify = \"observe\"\n").contains("[verify]"));
        let err = refusal("mode = \"observe\"\n");
        assert!(err.contains("mode belongs under [vetted_packages] or [verify]"), "{err}");
        let config = BaseConfig::test_minimal().unwrap();
        assert_eq!(config.verify, VerifySettings::default());
    }

    /// One table header of each kind the lenient pass reads, each with a key of its own.
    const LENIENT_TABLES: [(&str, &str); 12] = [
        ("[liquidity_provider]\nname = \"lp\"", "[liquidity_provider]"),
        ("[liquidity_provider]\nname = \"lp\"\n[liquidity_provider.rfq_v2]\nenabled = true", "[liquidity_provider.rfq_v2]"),
        (
            "[liquidity_provider]\nname = \"lp\"\n[liquidity_provider.rfq_v2.denominations]\nCC = [\"100x10\"]",
            "[liquidity_provider.rfq_v2.denominations]",
        ),
        ("[[markets]]\nmarket_id = \"CC-USDC\"", "[[markets]] (market_id = \"CC-USDC\")"),
        ("[[markets]]\nmarket_id = \"CC-USDC\"\n[markets.rfq]\nmin_quantity = \"1\"", "[markets.rfq] (market_id = \"CC-USDC\")"),
        (
            "[[markets]]\nmarket_id = \"CC-USDC\"\n[markets.rfq]\nmin_quantity = \"1\"\n[markets.rfq.v2]\nenabled = true",
            "[markets.rfq.v2] (market_id = \"CC-USDC\")",
        ),
        (
            "[[markets]]\nmarket_id = \"CC-USDC\"\n[markets.rfq.pool_impact]\nenabled = false",
            "[markets.rfq.pool_impact] (market_id = \"CC-USDC\")",
        ),
        (
            "[[markets]]\nmarket_id = \"CC-USDC\"\n[[markets.bid_levels]]\ndelta_percent = 1.0\nquantity = \"1\"",
            "[[markets.bid_levels]] (market_id = \"CC-USDC\", entry 1)",
        ),
        ("[[markets]]\nmarket_id = \"A-B\"\n[[markets]]\nenabled = true", "[[markets]] (entry 2)"),
        ("[[venue_overrides]]\nvenue = \"v\"", "[[venue_overrides]] (venue = \"v\")"),
        ("[[venue_overrides]]\nvenue = \"v\"\n[venue_overrides.rfq]\nenabled = true", "[venue_overrides.rfq] (venue = \"v\")"),
        ("[unknown_table]\nx = 1", "[unknown_table]"),
    ];

    #[test]
    fn a_section_key_below_a_lenient_table_is_refused_with_the_table_named() {
        for (header, at) in LENIENT_TABLES {
            let toml = format!("auto_settle = true\n{header}\n");
            assert!(parse_control_sections(&toml).is_ok(), "{toml}");
            for (key, _) in SECTION_KEYS {
                let toml = format!("auto_settle = true\n{header}\n{key} = true\n");
                let err = refusal(&toml);
                let expected = format!("{key} belongs under {}, not under {at} of agent.toml", sections_of(key));
                assert!(err.contains(&expected), "{toml}\n{err}");
            }
        }
        let err = refusal("[liquidity_provider]\nname = \"lp\"\nauto_execute_commands = true\n");
        assert!(err.starts_with("auto_execute_commands belongs under [control], not under [liquidity_provider]"), "{err}");
        // An inline table or array entry is read the same way.
        let err = refusal("[[markets]]\nmarket_id = \"X\"\nbid_levels = [{ delta_percent = 1.0, quantity = \"1\", network = \"devnet\" }]\n");
        assert!(err.contains("network belongs under [controller], not under [[markets.bid_levels]]"), "{err}");
    }

    #[test]
    fn the_strategy_table_and_the_strict_sections_keep_their_own_keys() {
        // `mode` is a key of `[markets.strategy]`, which refuses unknown keys itself.
        let toml = format!(
            "auto_settle = true\n[[markets]]\nmarket_id = \"CC-USDC\"\n[markets.strategy]\nkind = \"noise_taker\"\n\
             mode = \"maker\"\n[control]\nauto_execute_commands = true\n[vetted_packages]\nmode = \"selected\"\n\
             packages = [\"{ID_A}\"]\n[controller]\nkey_file = \"k\"\nagent_party = \"{PARTY}\"\nnetwork = \"devnet\"\n\
             [verify]\nmode = \"enforce\"\n"
        );
        let parsed = sections(&toml);
        assert!(parsed.control.auto_execute_commands);
        assert_eq!(parsed.verify.mode, VerifyModeSetting::Enforce);
        assert_eq!(parsed.vetted_packages.describe(), "selected (1 packages)");
    }

    /// Reads the agent.toml files listed in `AGENT_TOML_FILES` (one path per line) with the
    /// strict pass; none is listed by default.
    #[test]
    #[ignore]
    #[allow(clippy::disallowed_methods)]
    fn listed_agent_toml_files_pass_the_strict_pass() {
        let list = std::env::var("AGENT_TOML_FILES").unwrap_or_default();
        let mut read = 0;
        for path in list.lines().map(str::trim).filter(|p| !p.is_empty()) {
            let text = std::fs::read_to_string(path).unwrap();
            if let Err(e) = parse_control_sections(&text) {
                panic!("{path}: {e:#}");
            }
            read += 1;
        }
        println!("{read} agent.toml files pass");
    }

    #[test]
    fn every_key_the_sections_take_is_refused_at_the_top_level() {
        for section in ["control", "vetted_packages", "controller", "verify"] {
            // The strict sections name the keys they take.
            let err = refusal(&format!("[{section}]\nbogus = 1\n"));
            let taken: Vec<&str> = err.split('`').skip(1).step_by(2).filter(|key| *key != "bogus").collect();
            assert!(!taken.is_empty(), "{err}");
            for key in taken {
                assert!(SECTION_KEYS.contains(&(key, section)), "{key} of [{section}] is not checked: {err}");
            }
        }
    }

    #[test]
    fn vetted_packages_take_all_or_one_to_256_unique_ids() {
        let parsed = sections(&format!("[vetted_packages]\nmode = \"selected\"\npackages = [\"{ID_B}\", \"{ID_A}\"]\n"));
        let VettedPackages::Selected(ids) = &parsed.vetted_packages else { panic!("{parsed:?}") };
        assert_eq!(ids.iter().map(ToString::to_string).collect::<Vec<_>>(), vec![ID_A.to_string(), ID_B.to_string()]);
        assert_eq!(parsed.vetted_packages.describe(), "selected (2 packages)");

        let cases = [
            (format!("mode = \"all\"\npackages = [\"{ID_A}\"]"), "takes no packages"),
            ("mode = \"selected\"".to_string(), "needs a packages list"),
            ("mode = \"selected\"\npackages = []".to_string(), "1 to 256"),
            (format!("mode = \"selected\"\npackages = [\"{ID_A}\", \"{ID_A}\"]"), "listed twice"),
            (format!("mode = \"selected\"\npackages = [\"{}\"]", ID_B.to_ascii_uppercase()), "not a package id"),
            (format!("mode = \"selected\"\npackages = [\"{}\"]", &ID_A[1..]), "not a package id"),
            ("mode = \"some\"".to_string(), "neither"),
            ("packages = []".to_string(), "mode"),
        ];
        for (body, expected) in cases {
            let err = refusal(&format!("[vetted_packages]\n{body}\n"));
            assert!(err.contains("[vetted_packages]") && err.contains(expected), "{body}: {err}");
        }
        let many: Vec<String> = (0..257u32).map(|i| format!("\"{i:064x}\"")).collect();
        let err = refusal(&format!("[vetted_packages]\nmode = \"selected\"\npackages = [{}]\n", many.join(", ")));
        assert!(err.contains("not 257"), "{err}");
        let most: Vec<String> = (0..256u32).map(|i| format!("\"{i:064x}\"")).collect();
        let parsed = sections(&format!("[vetted_packages]\nmode = \"selected\"\npackages = [{}]\n", most.join(", ")));
        assert_eq!(parsed.vetted_packages.describe(), "selected (256 packages)");
    }

    #[test]
    fn the_section_the_agent_page_copies_parses() {
        let page = format!(
            "[vetted_packages]\nmode = \"selected\"\npackages = [\n  \"{ID_A}\", # splice-amulet 0.1.20\n  \"{ID_B}\", # utility-registry 1.2.3\n]\n"
        );
        let VettedPackages::Selected(ids) = sections(&page).vetted_packages else { panic!("{page}") };
        assert_eq!(ids.len(), 2);
        assert_eq!(sections("[vetted_packages]\nmode = \"all\"\n").vetted_packages, VettedPackages::All);
    }

    #[test]
    fn the_first_pass_stays_lenient_and_the_loader_refuses_a_bad_section() {
        let toml = "[control]\nbogus = 1\n[vetted_packages]\nmode = \"all\"\n";
        assert!(toml::from_str::<AgentToml>(toml).is_ok(), "older agents ignore the sections");
        let path = std::env::temp_dir().join(format!("agent-control-sections-{}.toml", std::process::id()));
        std::fs::write(&path, toml).unwrap();
        let strict = BaseConfig::load(&path).map(|_| ()).unwrap_err();
        let lenient = BaseConfig::load_or_defaults(&path).map(|_| ()).unwrap_err();
        let _ = std::fs::remove_file(&path);
        for err in [strict, lenient] {
            let text = format!("{err:#}");
            assert!(text.contains("[control]") && text.contains("bogus"), "{text}");
        }
    }

    const PARTY: &str = "agent-1::1220aa00000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn the_controller_section_is_read_strictly() {
        assert_eq!(sections("[control]\n").controller, None);
        let parsed = sections(&format!(
            "[controller]\nkey_file = \"controller/controller.key\"\nagent_party = \"{PARTY}\"\nnetwork = \"devnet\"\n"
        ));
        let controller = parsed.controller.unwrap();
        assert_eq!(controller.key_file, PathBuf::from("controller/controller.key"));
        assert_eq!(controller.agent_party.as_str(), PARTY);
        assert_eq!(controller.network, Some(agent_control::Network::Devnet));
        let no_network = sections(&format!("[controller]\nkey_file = \"k\"\nagent_party = \"{PARTY}\"\n"));
        assert_eq!(no_network.controller.unwrap().network, None);
        let cases = [
            (format!("key_file = \"k\"\nagent_party = \"{PARTY}\"\nparty = \"x\""), "unknown field"),
            (format!("agent_party = \"{PARTY}\""), "key_file"),
            ("key_file = \"k\"".to_string(), "agent_party"),
            (format!("key_file = \" \"\nagent_party = \"{PARTY}\""), "key_file is empty"),
            ("key_file = \"k\"\nagent_party = \"nobody\"".to_string(), "not a party id"),
            (format!("key_file = \"k\"\nagent_party = \"{PARTY}\"\nnetwork = \"testnet\""), "neither"),
            (format!("key_file = 1\nagent_party = \"{PARTY}\""), "[controller]"),
        ];
        for (body, expected) in cases {
            let err = refusal(&format!("[controller]\n{body}\n"));
            assert!(err.contains("[controller]") && err.contains(expected), "{body}: {err}");
        }
        assert!(refusal("controller = \"x\"\n").contains("[controller]"));
    }

    #[test]
    fn the_controller_loader_reads_only_its_section_and_resolves_the_key_beside_agent_toml() {
        let dir = std::env::temp_dir().join(format!("agent-controller-section-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.toml");
        assert_eq!(load_controller_section(&path).unwrap(), None, "no file, no section");
        // Other sections are not read in controller mode, even broken ones.
        std::fs::write(
            &path,
            format!("[control]\nbogus = 1\n[controller]\nkey_file = \"keys/controller.key\"\nagent_party = \"{PARTY}\"\n"),
        )
        .unwrap();
        let section = load_controller_section(&path).unwrap().unwrap();
        assert_eq!(section.key_file, dir.join("keys/controller.key"));
        std::fs::write(&path, format!("[controller]\nkey_file = \"/abs/k\"\nagent_party = \"{PARTY}\"\n")).unwrap();
        assert_eq!(load_controller_section(&path).unwrap().unwrap().key_file, PathBuf::from("/abs/k"));
        std::fs::write(&path, "auto_settle = true\n").unwrap();
        assert_eq!(load_controller_section(&path).unwrap(), None);
        std::fs::write(&path, "[controller]\nkey_file = \"k\"\n").unwrap();
        assert!(format!("{:#}", load_controller_section(&path).unwrap_err()).contains("agent_party"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn an_agent_whose_controller_section_names_its_own_party_is_refused() {
        // The same DSO the other env tests set.
        unsafe { std::env::set_var("DSO", "dso::1220aa") };
        let toml = format!("[controller]\nkey_file = \"k\"\nagent_party = \"{PARTY}\"\n");
        let assemble = |party: &str| {
            let agent: AgentToml = toml::from_str("").unwrap();
            let overrides = ConfigOverrides { party: Some(party.to_string()), ..Default::default() };
            BaseConfig::assemble_with(agent, BTreeMap::new(), sections(&toml), overrides).map(|_| ())
        };
        let own = format!("{:#}", assemble(PARTY).unwrap_err());
        assert!(own.contains("names this agent's own party"), "{own}");
        let own_padded = format!("{:#}", assemble(&format!(" {PARTY} ")).unwrap_err());
        assert!(own_padded.contains("names this agent's own party"), "{own_padded}");
        // Another party passes this check, whatever the rest of the environment holds.
        if let Err(other) = assemble("other::1220bb") {
            assert!(!format!("{other:#}").contains("names this agent's own party"), "{other:#}");
        }
    }
}
