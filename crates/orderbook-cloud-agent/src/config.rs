//! Configuration for the cloud orderbook agent
//!
//! Uses `BaseConfig` from `orderbook-agent-logic` directly — no ledger URLs needed
//! since all ledger operations go through the LedgerGatewayService gRPC proxy.

use anyhow::Result;
use std::path::Path;

use orderbook_agent_logic::config::BaseConfig;

/// Load cloud agent configuration from .env + configuration.toml + agent.toml
///
/// Identical to `BaseConfig::load()` — the cloud agent does not require
/// `CANTON_LEDGER_API_URL`, `CANTON_SCAN_API_URL`, or `CANTON_LEDGER_GRPC_API_URL`.
pub fn load<P: AsRef<Path>>(agent_toml_path: P) -> Result<BaseConfig> {
    BaseConfig::load(agent_toml_path)
}
