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

pub mod auth;
pub mod client;
pub mod confirm;
pub mod config;
pub mod forecast;
pub mod logging;
pub mod order_manager;
pub mod order_tracker;
pub mod rpc_client;
pub mod runner;
pub mod settlement;
pub mod sign;
pub mod state;
pub mod types;
