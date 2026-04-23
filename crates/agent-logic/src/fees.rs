//! Fee list helpers shared between the step-by-step LP settlement and the
//! taker-side multicall settlement path.
//!
//! Single source of truth for "which fees does a taker owe for one Accept_Dvp
//! + Allocate settlement": mirrors the LP agent's `queue_step_fees` ordering,
//! applied twice (once for step="accept", once for step="allocate"), plus the
//! DVP/allocation processing fees at the front.
//!
//! Fee amounts come from env vars (loaded into `BaseConfig`):
//! - `AGENT_FEE_CC`
//! - `PARTICIPANT_FEE_CC`
//! - `SIGNATURE_FEE_CC`
//!
//! Recipients:
//! - `fee_party` (`PARTY_ORDERBOOK_FEE`) for agent + signature + DVP + allocation fees
//! - `traffic_fee_party` (`PARTY_TRAFFIC_FEE`) for participant fees

use crate::config::BaseConfig;

/// A single fee target in a multicall `BatchTransfer` (or equivalent).
#[derive(Debug, Clone)]
pub struct FeeTarget {
    pub receiver: String,
    pub amount_cc: String,
    pub description: String,
}

/// Fees owed by the taker for a single Accept_Dvp + Allocate settlement,
/// in the order they should appear in an `ExecuteMultiCall` BatchTransfer.
///
/// Traffic fee is intentionally not included — the caller appends it after
/// traffic-size estimation on the prepared transaction.
///
/// Matches [`payment_queue::queue_step_fees`] semantics (one agent + one
/// participant + one signature per step, only if the corresponding env var
/// is set), applied for both "accept" and "allocate" steps, plus the two
/// DVP/allocation processing fees at the front.
pub fn taker_settlement_fees(
    cfg: &BaseConfig,
    dvp_processing_fee_cc: &str,
    allocation_processing_fee_cc: &str,
) -> Vec<FeeTarget> {
    let mut out = Vec::new();

    out.push(FeeTarget {
        receiver: cfg.fee_party.clone(),
        amount_cc: dvp_processing_fee_cc.to_string(),
        description: "DVP processing fee".to_string(),
    });
    out.push(FeeTarget {
        receiver: cfg.fee_party.clone(),
        amount_cc: allocation_processing_fee_cc.to_string(),
        description: "Allocation processing fee".to_string(),
    });

    for step in ["accept", "allocate"] {
        if let Some(ref agent) = cfg.agent_fee_cc {
            out.push(FeeTarget {
                receiver: cfg.fee_party.clone(),
                amount_cc: agent.clone(),
                description: format!("Agent fee ({})", step),
            });
        }
        if let Some(ref participant) = cfg.participant_fee_cc {
            out.push(FeeTarget {
                receiver: cfg.traffic_fee_party.clone(),
                amount_cc: participant.clone(),
                description: format!("Participant fee ({})", step),
            });
        }
        if let Some(ref sig) = cfg.signature_fee_cc {
            out.push(FeeTarget {
                receiver: cfg.fee_party.clone(),
                amount_cc: sig.clone(),
                description: format!("Signature fee ({})", step),
            });
        }
    }

    out
}
