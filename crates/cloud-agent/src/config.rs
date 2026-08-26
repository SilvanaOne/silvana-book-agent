//! Configuration for the cloud orderbook agent
//!
//! Uses `BaseConfig` from `orderbook-agent-logic` directly — no ledger URLs needed
//! since all ledger operations go through the LedgerGatewayService gRPC proxy.

use anyhow::Result;
use std::path::Path;

use agent_logic::config::{BaseConfig, ConfigOverrides};

/// Strict loader — `agent.toml` must exist. Use for commands that read
/// market/LP config (`agent`).
pub fn load<P: AsRef<Path>>(agent_toml_path: P) -> Result<BaseConfig> {
    BaseConfig::load(agent_toml_path)
}

/// [`load`] with CLI-supplied overrides for the env-sourced identity fields.
pub fn load_with<P: AsRef<Path>>(
    agent_toml_path: P,
    overrides: ConfigOverrides,
) -> Result<BaseConfig> {
    BaseConfig::load_with(agent_toml_path, overrides)
}

/// Lenient loader — missing `agent.toml` is OK (serde defaults fill in).
/// Use for commands that only touch env-sourced fields (faucet, transfer, etc.).
pub fn load_or_defaults<P: AsRef<Path>>(agent_toml_path: P) -> Result<BaseConfig> {
    BaseConfig::load_or_defaults(agent_toml_path)
}

/// [`load_or_defaults`] with CLI-supplied overrides for the env-sourced identity fields.
pub fn load_or_defaults_with<P: AsRef<Path>>(
    agent_toml_path: P,
    overrides: ConfigOverrides,
) -> Result<BaseConfig> {
    BaseConfig::load_or_defaults_with(agent_toml_path, overrides)
}
