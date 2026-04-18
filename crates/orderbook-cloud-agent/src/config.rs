//! Configuration for the cloud orderbook agent
//!
//! Uses `BaseConfig` from `orderbook-agent-logic` directly — no ledger URLs needed
//! since all ledger operations go through the LedgerGatewayService gRPC proxy.

use anyhow::Result;
use std::path::Path;

use orderbook_agent_logic::config::BaseConfig;

/// Strict loader — `agent.toml` must exist. Use for commands that read
/// market/LP config (`agent`).
pub fn load<P: AsRef<Path>>(agent_toml_path: P) -> Result<BaseConfig> {
    BaseConfig::load(agent_toml_path)
}

/// Lenient loader — missing `agent.toml` is OK (serde defaults fill in).
/// Use for commands that only touch env-sourced fields (faucet, transfer, etc.).
pub fn load_or_defaults<P: AsRef<Path>>(agent_toml_path: P) -> Result<BaseConfig> {
    BaseConfig::load_or_defaults(agent_toml_path)
}
