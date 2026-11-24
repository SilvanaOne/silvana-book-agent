//! Every operation the agent key can sign, and the policy class it is judged under.

use std::fmt;

/// The policy class of an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OperationClass {
    Maintenance,
    Trading,
    Transfers,
    Locks,
    Usdcx,
    FeeConsents,
    Identity,
    /// Judged through its sub-operations.
    Container,
}

impl OperationClass {
    pub const ALL: [Self; 8] = [
        Self::Maintenance,
        Self::Trading,
        Self::Transfers,
        Self::Locks,
        Self::Usdcx,
        Self::FeeConsents,
        Self::Identity,
        Self::Container,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Maintenance => "maintenance",
            Self::Trading => "trading",
            Self::Transfers => "transfers",
            Self::Locks => "locks",
            Self::Usdcx => "usdcx",
            Self::FeeConsents => "fee_consents",
            Self::Identity => "identity",
            Self::Container => "container",
        }
    }
}

impl fmt::Display for OperationClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ledger operations 1-27, the atomic operations, the multicall sub-operations and
/// the off-ledger signatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OperationKind {
    PayDvpFee,
    ProposeDvp,
    AcceptDvp,
    PayAllocFee,
    Allocate,
    TransferCc,
    RequestPreapproval,
    RequestRecurringPrepaid,
    RequestRecurringPayasyougo,
    RequestUserService,
    TransferCip56,
    AcceptCip56,
    SplitCc,
    ExecuteMultiCall,
    LockHoldings,
    ProcessLockUnlockRequests,
    ResizeLock,
    TerminateLock,
    PrepayTraffic,
    CancelDvpProposal,
    RejectDvpProposal,
    UsdcxRequestAgreement,
    UsdcxMint,
    UsdcxBurn,
    UsdcxReclaimHolding,
    UsdcxUpdatePreApproval,
    UsdcxTerminateAgreement,
    AtomicDvpSettle,
    IssueTickets,
    SplitHoldings,
    CreateAtomicDvpVenue,
    UpdateVenueKey,
    CancelTickets,
    RetireVenue,
    McBatchTransfer,
    McTransfer,
    McAcceptTransferInstruction,
    McAllocate,
    McWithdrawAllocation,
    McProposeDvp,
    McAcceptDvp,
    McAcceptDvpAndAllocate,
    McCreateTransferPreapproval,
    McCreateUserServiceRequest,
    McBatchPay,
    TxFeesAuthorization,
    PayFeeAuthorization,
    OffChainAuthorization,
    AtomicQuote,
    OrderSignature,
    Faucet,
    Jwt,
    RequestSignature,
    OnboardingTopology,
}

impl OperationKind {
    pub const ALL: [Self; 54] = [
        Self::PayDvpFee,
        Self::ProposeDvp,
        Self::AcceptDvp,
        Self::PayAllocFee,
        Self::Allocate,
        Self::TransferCc,
        Self::RequestPreapproval,
        Self::RequestRecurringPrepaid,
        Self::RequestRecurringPayasyougo,
        Self::RequestUserService,
        Self::TransferCip56,
        Self::AcceptCip56,
        Self::SplitCc,
        Self::ExecuteMultiCall,
        Self::LockHoldings,
        Self::ProcessLockUnlockRequests,
        Self::ResizeLock,
        Self::TerminateLock,
        Self::PrepayTraffic,
        Self::CancelDvpProposal,
        Self::RejectDvpProposal,
        Self::UsdcxRequestAgreement,
        Self::UsdcxMint,
        Self::UsdcxBurn,
        Self::UsdcxReclaimHolding,
        Self::UsdcxUpdatePreApproval,
        Self::UsdcxTerminateAgreement,
        Self::AtomicDvpSettle,
        Self::IssueTickets,
        Self::SplitHoldings,
        Self::CreateAtomicDvpVenue,
        Self::UpdateVenueKey,
        Self::CancelTickets,
        Self::RetireVenue,
        Self::McBatchTransfer,
        Self::McTransfer,
        Self::McAcceptTransferInstruction,
        Self::McAllocate,
        Self::McWithdrawAllocation,
        Self::McProposeDvp,
        Self::McAcceptDvp,
        Self::McAcceptDvpAndAllocate,
        Self::McCreateTransferPreapproval,
        Self::McCreateUserServiceRequest,
        Self::McBatchPay,
        Self::TxFeesAuthorization,
        Self::PayFeeAuthorization,
        Self::OffChainAuthorization,
        Self::AtomicQuote,
        Self::OrderSignature,
        Self::Faucet,
        Self::Jwt,
        Self::RequestSignature,
        Self::OnboardingTopology,
    ];

    /// The ledger `TransactionOperation` value; 0 and unknown values give `None`.
    pub fn from_transaction_operation(value: i32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.transaction_operation() == Some(value))
    }

    /// The stable name.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The class before the self-transfer and platform-party rules of the evaluator.
    pub fn class(self) -> OperationClass {
        use OperationClass as C;
        match self {
            Self::PayDvpFee
            | Self::ProposeDvp
            | Self::AcceptDvp
            | Self::PayAllocFee
            | Self::Allocate
            | Self::CancelDvpProposal
            | Self::RejectDvpProposal
            | Self::AtomicDvpSettle
            | Self::McAllocate
            | Self::McWithdrawAllocation
            | Self::McProposeDvp
            | Self::McAcceptDvp
            | Self::McAcceptDvpAndAllocate
            | Self::AtomicQuote
            | Self::OrderSignature => C::Trading,
            Self::TransferCc
            | Self::RequestRecurringPrepaid
            | Self::RequestRecurringPayasyougo
            | Self::TransferCip56
            | Self::UsdcxBurn
            | Self::McBatchTransfer
            | Self::McTransfer
            | Self::McBatchPay => C::Transfers,
            Self::RequestPreapproval
            | Self::RequestUserService
            | Self::AcceptCip56
            | Self::SplitCc
            | Self::PrepayTraffic
            | Self::IssueTickets
            | Self::SplitHoldings
            | Self::CancelTickets
            | Self::McAcceptTransferInstruction
            | Self::McCreateTransferPreapproval
            | Self::McCreateUserServiceRequest
            | Self::Faucet => C::Maintenance,
            Self::LockHoldings
            | Self::ProcessLockUnlockRequests
            | Self::ResizeLock
            | Self::TerminateLock
            | Self::CreateAtomicDvpVenue
            | Self::UpdateVenueKey
            | Self::RetireVenue => C::Locks,
            Self::UsdcxRequestAgreement
            | Self::UsdcxMint
            | Self::UsdcxReclaimHolding
            | Self::UsdcxUpdatePreApproval
            | Self::UsdcxTerminateAgreement => C::Usdcx,
            Self::TxFeesAuthorization | Self::PayFeeAuthorization | Self::OffChainAuthorization => {
                C::FeeConsents
            }
            Self::Jwt | Self::RequestSignature | Self::OnboardingTopology => C::Identity,
            Self::ExecuteMultiCall => C::Container,
        }
    }

    /// Winding down: passes a pause and a disabled class, never a cap.
    pub fn is_unwind(self) -> bool {
        match self {
            Self::TerminateLock
            | Self::CancelDvpProposal
            | Self::RejectDvpProposal
            | Self::UsdcxReclaimHolding
            | Self::CancelTickets
            | Self::UpdateVenueKey
            | Self::RetireVenue
            | Self::McWithdrawAllocation => true,
            Self::PayDvpFee
            | Self::ProposeDvp
            | Self::AcceptDvp
            | Self::PayAllocFee
            | Self::Allocate
            | Self::TransferCc
            | Self::RequestPreapproval
            | Self::RequestRecurringPrepaid
            | Self::RequestRecurringPayasyougo
            | Self::RequestUserService
            | Self::TransferCip56
            | Self::AcceptCip56
            | Self::SplitCc
            | Self::ExecuteMultiCall
            | Self::LockHoldings
            | Self::ProcessLockUnlockRequests
            | Self::ResizeLock
            | Self::PrepayTraffic
            | Self::UsdcxRequestAgreement
            | Self::UsdcxMint
            | Self::UsdcxBurn
            | Self::UsdcxUpdatePreApproval
            | Self::UsdcxTerminateAgreement
            | Self::AtomicDvpSettle
            | Self::IssueTickets
            | Self::SplitHoldings
            | Self::CreateAtomicDvpVenue
            | Self::McBatchTransfer
            | Self::McTransfer
            | Self::McAcceptTransferInstruction
            | Self::McAllocate
            | Self::McProposeDvp
            | Self::McAcceptDvp
            | Self::McAcceptDvpAndAllocate
            | Self::McCreateTransferPreapproval
            | Self::McCreateUserServiceRequest
            | Self::McBatchPay
            | Self::TxFeesAuthorization
            | Self::PayFeeAuthorization
            | Self::OffChainAuthorization
            | Self::AtomicQuote
            | Self::OrderSignature
            | Self::Faucet
            | Self::Jwt
            | Self::RequestSignature
            | Self::OnboardingTopology => false,
        }
    }

    /// Needs `trading.lp_enabled`.
    pub fn requires_lp(self) -> bool {
        match self {
            Self::IssueTickets | Self::CreateAtomicDvpVenue | Self::AtomicQuote => true,
            Self::PayDvpFee
            | Self::ProposeDvp
            | Self::AcceptDvp
            | Self::PayAllocFee
            | Self::Allocate
            | Self::TransferCc
            | Self::RequestPreapproval
            | Self::RequestRecurringPrepaid
            | Self::RequestRecurringPayasyougo
            | Self::RequestUserService
            | Self::TransferCip56
            | Self::AcceptCip56
            | Self::SplitCc
            | Self::ExecuteMultiCall
            | Self::LockHoldings
            | Self::ProcessLockUnlockRequests
            | Self::ResizeLock
            | Self::TerminateLock
            | Self::PrepayTraffic
            | Self::CancelDvpProposal
            | Self::RejectDvpProposal
            | Self::UsdcxRequestAgreement
            | Self::UsdcxMint
            | Self::UsdcxBurn
            | Self::UsdcxReclaimHolding
            | Self::UsdcxUpdatePreApproval
            | Self::UsdcxTerminateAgreement
            | Self::AtomicDvpSettle
            | Self::SplitHoldings
            | Self::UpdateVenueKey
            | Self::CancelTickets
            | Self::RetireVenue
            | Self::McBatchTransfer
            | Self::McTransfer
            | Self::McAcceptTransferInstruction
            | Self::McAllocate
            | Self::McWithdrawAllocation
            | Self::McProposeDvp
            | Self::McAcceptDvp
            | Self::McAcceptDvpAndAllocate
            | Self::McCreateTransferPreapproval
            | Self::McCreateUserServiceRequest
            | Self::McBatchPay
            | Self::TxFeesAuthorization
            | Self::PayFeeAuthorization
            | Self::OffChainAuthorization
            | Self::OrderSignature
            | Self::Faucet
            | Self::Jwt
            | Self::RequestSignature
            | Self::OnboardingTopology => false,
        }
    }

    /// Maintenance instead of a transfer when every flow goes back to the agent.
    pub fn is_self_transferable(self) -> bool {
        match self {
            Self::TransferCip56 | Self::McTransfer => true,
            Self::PayDvpFee
            | Self::ProposeDvp
            | Self::AcceptDvp
            | Self::PayAllocFee
            | Self::Allocate
            | Self::TransferCc
            | Self::RequestPreapproval
            | Self::RequestRecurringPrepaid
            | Self::RequestRecurringPayasyougo
            | Self::RequestUserService
            | Self::AcceptCip56
            | Self::SplitCc
            | Self::ExecuteMultiCall
            | Self::LockHoldings
            | Self::ProcessLockUnlockRequests
            | Self::ResizeLock
            | Self::TerminateLock
            | Self::PrepayTraffic
            | Self::CancelDvpProposal
            | Self::RejectDvpProposal
            | Self::UsdcxRequestAgreement
            | Self::UsdcxMint
            | Self::UsdcxBurn
            | Self::UsdcxReclaimHolding
            | Self::UsdcxUpdatePreApproval
            | Self::UsdcxTerminateAgreement
            | Self::AtomicDvpSettle
            | Self::IssueTickets
            | Self::SplitHoldings
            | Self::CreateAtomicDvpVenue
            | Self::UpdateVenueKey
            | Self::CancelTickets
            | Self::RetireVenue
            | Self::McBatchTransfer
            | Self::McAcceptTransferInstruction
            | Self::McAllocate
            | Self::McWithdrawAllocation
            | Self::McProposeDvp
            | Self::McAcceptDvp
            | Self::McAcceptDvpAndAllocate
            | Self::McCreateTransferPreapproval
            | Self::McCreateUserServiceRequest
            | Self::McBatchPay
            | Self::TxFeesAuthorization
            | Self::PayFeeAuthorization
            | Self::OffChainAuthorization
            | Self::AtomicQuote
            | Self::OrderSignature
            | Self::Faucet
            | Self::Jwt
            | Self::RequestSignature
            | Self::OnboardingTopology => false,
        }
    }

    /// One of the operations inside an `ExecuteMultiCall`.
    pub fn is_multicall_sub(self) -> bool {
        match self {
            Self::McBatchTransfer
            | Self::McTransfer
            | Self::McAcceptTransferInstruction
            | Self::McAllocate
            | Self::McWithdrawAllocation
            | Self::McProposeDvp
            | Self::McAcceptDvp
            | Self::McAcceptDvpAndAllocate
            | Self::McCreateTransferPreapproval
            | Self::McCreateUserServiceRequest
            | Self::McBatchPay => true,
            Self::PayDvpFee
            | Self::ProposeDvp
            | Self::AcceptDvp
            | Self::PayAllocFee
            | Self::Allocate
            | Self::TransferCc
            | Self::RequestPreapproval
            | Self::RequestRecurringPrepaid
            | Self::RequestRecurringPayasyougo
            | Self::RequestUserService
            | Self::TransferCip56
            | Self::AcceptCip56
            | Self::SplitCc
            | Self::ExecuteMultiCall
            | Self::LockHoldings
            | Self::ProcessLockUnlockRequests
            | Self::ResizeLock
            | Self::TerminateLock
            | Self::PrepayTraffic
            | Self::CancelDvpProposal
            | Self::RejectDvpProposal
            | Self::UsdcxRequestAgreement
            | Self::UsdcxMint
            | Self::UsdcxBurn
            | Self::UsdcxReclaimHolding
            | Self::UsdcxUpdatePreApproval
            | Self::UsdcxTerminateAgreement
            | Self::AtomicDvpSettle
            | Self::IssueTickets
            | Self::SplitHoldings
            | Self::CreateAtomicDvpVenue
            | Self::UpdateVenueKey
            | Self::CancelTickets
            | Self::RetireVenue
            | Self::TxFeesAuthorization
            | Self::PayFeeAuthorization
            | Self::OffChainAuthorization
            | Self::AtomicQuote
            | Self::OrderSignature
            | Self::Faucet
            | Self::Jwt
            | Self::RequestSignature
            | Self::OnboardingTopology => false,
        }
    }

    /// The ledger `TransactionOperation` value, for operations 1-27.
    pub fn transaction_operation(self) -> Option<i32> {
        match self {
            Self::PayDvpFee => Some(1),
            Self::ProposeDvp => Some(2),
            Self::AcceptDvp => Some(3),
            Self::PayAllocFee => Some(4),
            Self::Allocate => Some(5),
            Self::TransferCc => Some(6),
            Self::RequestPreapproval => Some(7),
            Self::RequestRecurringPrepaid => Some(8),
            Self::RequestRecurringPayasyougo => Some(9),
            Self::RequestUserService => Some(10),
            Self::TransferCip56 => Some(11),
            Self::AcceptCip56 => Some(12),
            Self::SplitCc => Some(13),
            Self::ExecuteMultiCall => Some(14),
            Self::LockHoldings => Some(15),
            Self::ProcessLockUnlockRequests => Some(16),
            Self::ResizeLock => Some(17),
            Self::TerminateLock => Some(18),
            Self::PrepayTraffic => Some(19),
            Self::CancelDvpProposal => Some(20),
            Self::RejectDvpProposal => Some(21),
            Self::UsdcxRequestAgreement => Some(22),
            Self::UsdcxMint => Some(23),
            Self::UsdcxBurn => Some(24),
            Self::UsdcxReclaimHolding => Some(25),
            Self::UsdcxUpdatePreApproval => Some(26),
            Self::UsdcxTerminateAgreement => Some(27),
            Self::AtomicDvpSettle
            | Self::IssueTickets
            | Self::SplitHoldings
            | Self::CreateAtomicDvpVenue
            | Self::UpdateVenueKey
            | Self::CancelTickets
            | Self::RetireVenue
            | Self::McBatchTransfer
            | Self::McTransfer
            | Self::McAcceptTransferInstruction
            | Self::McAllocate
            | Self::McWithdrawAllocation
            | Self::McProposeDvp
            | Self::McAcceptDvp
            | Self::McAcceptDvpAndAllocate
            | Self::McCreateTransferPreapproval
            | Self::McCreateUserServiceRequest
            | Self::McBatchPay
            | Self::TxFeesAuthorization
            | Self::PayFeeAuthorization
            | Self::OffChainAuthorization
            | Self::AtomicQuote
            | Self::OrderSignature
            | Self::Faucet
            | Self::Jwt
            | Self::RequestSignature
            | Self::OnboardingTopology => None,
        }
    }

    /// Stable snake_case name.
    pub fn name(self) -> &'static str {
        match self {
            Self::PayDvpFee => "pay_dvp_fee",
            Self::ProposeDvp => "propose_dvp",
            Self::AcceptDvp => "accept_dvp",
            Self::PayAllocFee => "pay_alloc_fee",
            Self::Allocate => "allocate",
            Self::TransferCc => "transfer_cc",
            Self::RequestPreapproval => "request_preapproval",
            Self::RequestRecurringPrepaid => "request_recurring_prepaid",
            Self::RequestRecurringPayasyougo => "request_recurring_payasyougo",
            Self::RequestUserService => "request_user_service",
            Self::TransferCip56 => "transfer_cip56",
            Self::AcceptCip56 => "accept_cip56",
            Self::SplitCc => "split_cc",
            Self::ExecuteMultiCall => "execute_multicall",
            Self::LockHoldings => "lock_holdings",
            Self::ProcessLockUnlockRequests => "process_lock_unlock_requests",
            Self::ResizeLock => "resize_lock",
            Self::TerminateLock => "terminate_lock",
            Self::PrepayTraffic => "prepay_traffic",
            Self::CancelDvpProposal => "cancel_dvp_proposal",
            Self::RejectDvpProposal => "reject_dvp_proposal",
            Self::UsdcxRequestAgreement => "usdcx_request_agreement",
            Self::UsdcxMint => "usdcx_mint",
            Self::UsdcxBurn => "usdcx_burn",
            Self::UsdcxReclaimHolding => "usdcx_reclaim_holding",
            Self::UsdcxUpdatePreApproval => "usdcx_update_pre_approval",
            Self::UsdcxTerminateAgreement => "usdcx_terminate_agreement",
            Self::AtomicDvpSettle => "atomic_dvp_settle",
            Self::IssueTickets => "issue_tickets",
            Self::SplitHoldings => "split_holdings",
            Self::CreateAtomicDvpVenue => "create_atomic_dvp_venue",
            Self::UpdateVenueKey => "update_venue_key",
            Self::CancelTickets => "cancel_tickets",
            Self::RetireVenue => "retire_venue",
            Self::McBatchTransfer => "mc_batch_transfer",
            Self::McTransfer => "mc_transfer",
            Self::McAcceptTransferInstruction => "mc_accept_transfer_instruction",
            Self::McAllocate => "mc_allocate",
            Self::McWithdrawAllocation => "mc_withdraw_allocation",
            Self::McProposeDvp => "mc_propose_dvp",
            Self::McAcceptDvp => "mc_accept_dvp",
            Self::McAcceptDvpAndAllocate => "mc_accept_dvp_and_allocate",
            Self::McCreateTransferPreapproval => "mc_create_transfer_preapproval",
            Self::McCreateUserServiceRequest => "mc_create_user_service_request",
            Self::McBatchPay => "mc_batch_pay",
            Self::TxFeesAuthorization => "tx_fees_authorization",
            Self::PayFeeAuthorization => "pay_fee_authorization",
            Self::OffChainAuthorization => "off_chain_authorization",
            Self::AtomicQuote => "atomic_quote",
            Self::OrderSignature => "order_signature",
            Self::Faucet => "faucet",
            Self::Jwt => "jwt",
            Self::RequestSignature => "request_signature",
            Self::OnboardingTopology => "onboarding_topology",
        }
    }
}

impl fmt::Display for OperationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OperationClass as C;
    use OperationKind as K;
    use std::collections::HashSet;

    /// kind, class, unwind, requires LP.
    const TABLE: [(K, C, bool, bool); 54] = [
        (K::PayDvpFee, C::Trading, false, false),
        (K::ProposeDvp, C::Trading, false, false),
        (K::AcceptDvp, C::Trading, false, false),
        (K::PayAllocFee, C::Trading, false, false),
        (K::Allocate, C::Trading, false, false),
        (K::TransferCc, C::Transfers, false, false),
        (K::RequestPreapproval, C::Maintenance, false, false),
        (K::RequestRecurringPrepaid, C::Transfers, false, false),
        (K::RequestRecurringPayasyougo, C::Transfers, false, false),
        (K::RequestUserService, C::Maintenance, false, false),
        (K::TransferCip56, C::Transfers, false, false),
        (K::AcceptCip56, C::Maintenance, false, false),
        (K::SplitCc, C::Maintenance, false, false),
        (K::ExecuteMultiCall, C::Container, false, false),
        (K::LockHoldings, C::Locks, false, false),
        (K::ProcessLockUnlockRequests, C::Locks, false, false),
        (K::ResizeLock, C::Locks, false, false),
        (K::TerminateLock, C::Locks, true, false),
        (K::PrepayTraffic, C::Maintenance, false, false),
        (K::CancelDvpProposal, C::Trading, true, false),
        (K::RejectDvpProposal, C::Trading, true, false),
        (K::UsdcxRequestAgreement, C::Usdcx, false, false),
        (K::UsdcxMint, C::Usdcx, false, false),
        (K::UsdcxBurn, C::Transfers, false, false),
        (K::UsdcxReclaimHolding, C::Usdcx, true, false),
        (K::UsdcxUpdatePreApproval, C::Usdcx, false, false),
        (K::UsdcxTerminateAgreement, C::Usdcx, false, false),
        (K::AtomicDvpSettle, C::Trading, false, false),
        (K::IssueTickets, C::Maintenance, false, true),
        (K::SplitHoldings, C::Maintenance, false, false),
        (K::CreateAtomicDvpVenue, C::Locks, false, true),
        (K::UpdateVenueKey, C::Locks, true, false),
        (K::CancelTickets, C::Maintenance, true, false),
        (K::RetireVenue, C::Locks, true, false),
        (K::McBatchTransfer, C::Transfers, false, false),
        (K::McTransfer, C::Transfers, false, false),
        (K::McAcceptTransferInstruction, C::Maintenance, false, false),
        (K::McAllocate, C::Trading, false, false),
        (K::McWithdrawAllocation, C::Trading, true, false),
        (K::McProposeDvp, C::Trading, false, false),
        (K::McAcceptDvp, C::Trading, false, false),
        (K::McAcceptDvpAndAllocate, C::Trading, false, false),
        (K::McCreateTransferPreapproval, C::Maintenance, false, false),
        (K::McCreateUserServiceRequest, C::Maintenance, false, false),
        (K::McBatchPay, C::Transfers, false, false),
        (K::TxFeesAuthorization, C::FeeConsents, false, false),
        (K::PayFeeAuthorization, C::FeeConsents, false, false),
        (K::OffChainAuthorization, C::FeeConsents, false, false),
        (K::AtomicQuote, C::Trading, false, true),
        (K::OrderSignature, C::Trading, false, false),
        (K::Faucet, C::Maintenance, false, false),
        (K::Jwt, C::Identity, false, false),
        (K::RequestSignature, C::Identity, false, false),
        (K::OnboardingTopology, C::Identity, false, false),
    ];

    #[test]
    fn every_kind_has_its_pinned_class_and_flags() {
        assert_eq!(TABLE.map(|row| row.0), K::ALL);
        for (kind, class, unwind, lp) in TABLE {
            assert_eq!(kind.class(), class, "{kind}");
            assert_eq!(kind.is_unwind(), unwind, "{kind}");
            assert_eq!(kind.requires_lp(), lp, "{kind}");
            assert_eq!(K::parse(kind.name()), Some(kind));
        }
        let names: HashSet<&str> = K::ALL.iter().map(|k| k.name()).collect();
        assert_eq!(names.len(), K::ALL.len());
        let subs: Vec<K> = K::ALL
            .into_iter()
            .filter(|k| k.is_multicall_sub())
            .collect();
        assert_eq!(subs.len(), 11);
        assert!(subs.iter().all(|k| k.name().starts_with("mc_")));
        let to_self: Vec<K> = K::ALL
            .into_iter()
            .filter(|k| k.is_self_transferable())
            .collect();
        assert_eq!(to_self, vec![K::TransferCip56, K::McTransfer]);
        assert_eq!(K::parse("unknown"), None);
        let classes: HashSet<&str> = C::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(classes.len(), C::ALL.len());
    }

    #[test]
    fn transaction_operations_cover_one_to_twenty_seven() {
        for value in 1..=27 {
            let kind = K::from_transaction_operation(value).unwrap();
            assert_eq!(kind.transaction_operation(), Some(value));
        }
        for value in [0, 28, -1, i32::MIN, i32::MAX] {
            assert_eq!(K::from_transaction_operation(value), None, "{value}");
        }
        let numbered = K::ALL
            .iter()
            .filter(|k| k.transaction_operation().is_some())
            .count();
        assert_eq!(numbered, 27);
        assert_eq!(K::from_transaction_operation(14), Some(K::ExecuteMultiCall));
        assert_eq!(K::from_transaction_operation(24), Some(K::UsdcxBurn));
    }
}
