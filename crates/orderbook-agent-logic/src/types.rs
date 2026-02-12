//! Shared types for the orderbook agent
//!
//! Contains settlement state tracking and related types.

use std::time::{Duration, Instant};

use orderbook_proto::orderbook::SettlementProposal;

/// Settlement stage in the DVP workflow
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementStage {
    /// Settlement proposal received
    ProposalReceived,
    /// DVP processing fee paid
    DvpFeePaid,
    /// DVP proposal created (buyer only)
    DvpProposed,
    /// DVP proposal accepted, Dvp contract created (seller accepted)
    DvpAccepted,
    /// Allocation processing fee paid
    AllocationFeePaid,
    /// Tokens allocated for settlement
    Allocated,
    /// Waiting for settlement operator to execute
    AwaitingSettlement,
    /// Settlement completed successfully
    Settled,
    /// Settlement failed
    Failed,
    /// Settlement cancelled
    Cancelled,
}

impl std::fmt::Display for SettlementStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettlementStage::ProposalReceived => write!(f, "ProposalReceived"),
            SettlementStage::DvpFeePaid => write!(f, "DvpFeePaid"),
            SettlementStage::DvpProposed => write!(f, "DvpProposed"),
            SettlementStage::DvpAccepted => write!(f, "DvpAccepted"),
            SettlementStage::AllocationFeePaid => write!(f, "AllocationFeePaid"),
            SettlementStage::Allocated => write!(f, "Allocated"),
            SettlementStage::AwaitingSettlement => write!(f, "AwaitingSettlement"),
            SettlementStage::Settled => write!(f, "Settled"),
            SettlementStage::Failed => write!(f, "Failed"),
            SettlementStage::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// State tracking for an active settlement
#[derive(Debug, Clone)]
pub struct SettlementState {
    /// The settlement proposal
    pub proposal: SettlementProposal,
    /// Whether this party is the buyer
    pub is_buyer: bool,
    /// Current stage in the settlement workflow
    pub stage: SettlementStage,
    /// DvpProposal contract ID (set after propose)
    pub dvp_proposal_cid: Option<String>,
    /// Dvp contract ID (set after accept)
    pub dvp_cid: Option<String>,
    /// Allocation contract ID (set after allocate)
    pub allocation_cid: Option<String>,
    /// Pending traffic bytes from fee step (for JOIN_TRAFFIC_TRANSACTIONS)
    pub pending_traffic: u64,
}

impl SettlementState {
    /// Create a new settlement state
    pub fn new(proposal: SettlementProposal, is_buyer: bool) -> Self {
        Self {
            proposal,
            is_buyer,
            stage: SettlementStage::ProposalReceived,
            dvp_proposal_cid: None,
            dvp_cid: None,
            allocation_cid: None,
            pending_traffic: 0,
        }
    }

    /// Get the proposal ID
    pub fn proposal_id(&self) -> &str {
        &self.proposal.proposal_id
    }

    /// Get the role description
    pub fn role(&self) -> &str {
        if self.is_buyer { "buyer" } else { "seller" }
    }
}

/// Result from a spawned settlement advancement task
pub enum AdvanceResult {
    /// Step completed — apply state updates to active_settlements
    StepCompleted {
        proposal_id: String,
        stage: SettlementStage,
        dvp_proposal_cid: Option<String>,
        dvp_cid: Option<String>,
        allocation_cid: Option<String>,
        pending_traffic: u64,
    },
    /// Preconfirmation submitted
    Preconfirmed { proposal_id: String },
    /// Rejected (during shutdown or verification failure)
    Rejected { proposal_id: String },
    /// Terminal state — tracker already updated in the task
    Terminal { proposal_id: String },
    /// Waiting for counterparty (no-op)
    Wait { proposal_id: String },
    /// Error during advancement
    Error { proposal_id: String, error: String },
    /// Timeout — likely transient, retry sooner (fixed 60s)
    Timeout { proposal_id: String },
}

impl AdvanceResult {
    /// Get the proposal ID from any variant
    pub fn proposal_id(&self) -> &str {
        match self {
            AdvanceResult::StepCompleted { proposal_id, .. }
            | AdvanceResult::Preconfirmed { proposal_id }
            | AdvanceResult::Rejected { proposal_id }
            | AdvanceResult::Terminal { proposal_id }
            | AdvanceResult::Wait { proposal_id }
            | AdvanceResult::Error { proposal_id, .. }
            | AdvanceResult::Timeout { proposal_id } => proposal_id,
        }
    }

    /// Whether this result means the settlement can advance further
    pub fn should_readvance(&self) -> bool {
        matches!(self, AdvanceResult::StepCompleted { .. } | AdvanceResult::Preconfirmed { .. })
    }

    /// Apply state changes from this result to a SettlementState (for re-advance loops)
    pub fn apply_to_state(&self, state: &mut SettlementState) {
        if let AdvanceResult::StepCompleted {
            stage,
            dvp_proposal_cid,
            dvp_cid,
            allocation_cid,
            pending_traffic,
            ..
        } = self
        {
            state.stage = *stage;
            if let Some(cid) = dvp_proposal_cid {
                state.dvp_proposal_cid = Some(cid.clone());
            }
            if let Some(cid) = dvp_cid {
                state.dvp_cid = Some(cid.clone());
            }
            if let Some(cid) = allocation_cid {
                state.allocation_cid = Some(cid.clone());
            }
            state.pending_traffic = *pending_traffic;
        }
    }
}

/// Failed settlement with retry backoff (matches server pattern)
pub struct FailedSettlement {
    pub retry_count: u32,
    pub next_retry: Instant,
}

impl FailedSettlement {
    pub fn retry_delay(retry_count: u32) -> Duration {
        match retry_count {
            0 => Duration::from_secs(0),
            1 => Duration::from_secs(60),
            2 => Duration::from_secs(300),
            _ => Duration::from_secs(1800),
        }
    }

    pub fn max_retries() -> u32 {
        std::env::var("MAX_SETTLEMENT_RETRIES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10)
    }

    pub fn is_exhausted(&self) -> bool {
        self.retry_count >= Self::max_retries()
    }
}
