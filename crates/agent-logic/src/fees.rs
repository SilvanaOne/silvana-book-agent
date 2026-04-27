//! Fee target struct (legacy).
//!
//! Historically this module produced `BatchTransfer` fee entries that were
//! embedded into the cloud-agent's settlement multicall. All fee categories
//! (agent / participant / signature / traffic / DVP-processing /
//! allocation-processing) have moved off-chain — the ledger-service issues a
//! signed schedule and debits the cloud-agent's prepaid traffic balance via
//! the `PrepaidTrafficStore` trait.
//!
//! What remains: the `FeeTarget` struct, retained as a placeholder type so
//! the cloud-agent's multicall builder continues to compile with an
//! always-empty fee list. Once the multicall builder drops its fee
//! parameter, this module can be deleted.

#[derive(Debug, Clone)]
pub struct FeeTarget {
    pub receiver: String,
    pub amount_cc: String,
    pub description: String,
}
