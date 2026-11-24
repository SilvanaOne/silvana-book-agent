//! An Ed25519 controller of linked agents: it signs in by signing an rpc challenge, verifies
//! each agent's log locally, and signs policies and commands drafted from that log.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    allow(renamed_and_removed_lints),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::unchecked_duration_subtraction,
        clippy::arithmetic_side_effects,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::dbg_macro,
        clippy::disallowed_methods
    ),
    warn(renamed_and_removed_lints)
)]

pub mod client;
pub mod clock;
pub mod error;
pub mod pin;
pub mod receipt;
pub mod signer;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
#[cfg(test)]
mod tests;

use std::time::Duration;

pub use client::{
    CommandDraft, ControllerClient, ControllerConfig, PolicyBase, PolicyDraft, Reported, SignedIn,
    Standing, SubmittedCommand, SubmittedPolicy, VerifiedAgent, Waited,
};
pub use error::ControllerError;
pub use pin::{PinCheck, SEEN_HEADS_FILE, SeenHeads};
pub use receipt::{ReceiptCheck, check_receipt};
pub use signer::{ControllerSigner, KEY_FILE, MAX_KEY_FILE_BYTES, SignedDocument};

/// Bound on one call, connection included.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on reading a whole log, every page included.
pub const LOG_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on ending a session.
pub const END_TIMEOUT: Duration = Duration::from_secs(3);
/// Largest response accepted.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Entries asked for per log page.
pub const LOG_PAGE_LIMIT: u32 = 256;
/// Default validity of a policy draft, in seconds; the longest is the crate's.
pub const POLICY_VALIDITY_SECS: u64 = 3600;
pub use agent_control::policy::MAX_POLICY_VALIDITY_SECS;
/// Default validity of a command draft, in seconds; the longest is the crate's.
pub const COMMAND_VALIDITY_SECS: u64 = 600;
pub use agent_control::command::MAX_COMMAND_VALIDITY_SECS;
/// Prefix of a session token.
pub const SESSION_TOKEN_PREFIX: &str = "sct1_";
/// Refusal codes: no usable session, and sign-in refused.
pub const NO_SESSION_CODE: u16 = 4096;
pub const SIGN_IN_REFUSED_CODE: u16 = 4097;
