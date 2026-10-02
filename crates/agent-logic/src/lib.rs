//! Orderbook Agent Logic - Shared settlement orchestration and order tracking
//!
//! This library contains the core logic shared between the local orderbook-agent
//! (direct ledger access) and the future cloud agent (LedgerGatewayService proxy).
//!
//! Key components:
//! - Settlement executor with pluggable backend (`SettlementBackend` trait)
//! - Order tracking and verification
//! - JWT authentication
//! - gRPC clients for orderbook and settlement services

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

pub mod auth;
pub mod client;
pub mod clock;
pub mod confirm;
pub mod config;
pub mod error_reporter;
pub mod fees;
pub mod forecast;
pub mod grid_task;
pub mod ledger_health;
pub mod liquidity;
pub mod logging;
pub mod net_position;
pub mod num;
pub mod order_manager;
pub mod order_tracker;
pub mod panic_hook;
pub mod pool_impact;
pub mod rpc_client;
pub mod runner;
pub mod secret;
pub mod settlement;
pub mod shutdown;
pub mod sign;
pub mod state;
pub mod stdio;
pub mod supervise;
pub mod sync;
#[cfg(test)]
mod test_logs;
pub mod transport;
pub mod types;
