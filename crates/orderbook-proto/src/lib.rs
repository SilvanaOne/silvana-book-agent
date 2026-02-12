//! Protobuf definitions and gRPC clients for Silvana Orderbook, Settlement, and Pricing services
//!
//! This crate provides Rust client interfaces for:
//! - OrderbookService: Market data, order management, and orderbook subscriptions
//! - SettlementService: DVP settlement coordination and streaming
//! - PricingService: Real-time price feeds and market data aggregation

use once_cell::sync::Lazy;
use prost_reflect::DescriptorPool;

/// Orderbook service protobuf definitions
pub mod orderbook {
    tonic::include_proto!("silvana.orderbook.v1");

    /// File descriptor set for orderbook service
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("orderbook_descriptor");
}

/// Settlement service protobuf definitions
pub mod settlement {
    tonic::include_proto!("silvana.settlement.v1");

    /// File descriptor set for settlement service
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("settlement_descriptor");
}

/// Pricing service protobuf definitions
pub mod pricing {
    tonic::include_proto!("silvana.pricing.v1");

    /// File descriptor set for pricing service
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("pricing_descriptor");
}

/// DApp Provider service (CIP-0103) protobuf definitions
pub mod ledger {
    tonic::include_proto!("silvana.ledger.v1");

    /// File descriptor set for DApp Provider service
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("ledger_descriptor");
}

/// Descriptor pool for orderbook service reflection
pub static ORDERBOOK_DESCRIPTOR_POOL: Lazy<DescriptorPool> =
    Lazy::new(|| DescriptorPool::decode(orderbook::FILE_DESCRIPTOR_SET.as_ref()).unwrap());

/// Descriptor pool for settlement service reflection
pub static SETTLEMENT_DESCRIPTOR_POOL: Lazy<DescriptorPool> =
    Lazy::new(|| DescriptorPool::decode(settlement::FILE_DESCRIPTOR_SET.as_ref()).unwrap());

/// Descriptor pool for pricing service reflection
pub static PRICING_DESCRIPTOR_POOL: Lazy<DescriptorPool> =
    Lazy::new(|| DescriptorPool::decode(pricing::FILE_DESCRIPTOR_SET.as_ref()).unwrap());

/// Descriptor pool for ledger gateway service reflection
pub static LEDGER_DESCRIPTOR_POOL: Lazy<DescriptorPool> =
    Lazy::new(|| DescriptorPool::decode(ledger::FILE_DESCRIPTOR_SET.as_ref()).unwrap());

// Re-export commonly used types for convenience
pub use orderbook::{
    orderbook_service_client::OrderbookServiceClient,
    GetMarketsRequest, GetMarketsResponse,
    GetInstrumentsRequest, GetInstrumentsResponse,
    GetOrderbookDepthRequest, GetOrderbookDepthResponse,
    GetOrdersRequest, GetOrdersResponse,
    SubmitOrderRequest, SubmitOrderResponse,
    CancelOrderRequest, CancelOrderResponse,
    SubscribeOrderbookRequest, OrderbookUpdate,
    SubscribeOrdersRequest, OrderUpdate,
    SubscribeSettlementsRequest, SettlementUpdate,
    CreatePartyRequest, CreatePartyResponse, Party,
    UpdatePartyRequest, UpdatePartyResponse,
    GetPartyRequest, GetPartyResponse,
    CreateInstrumentRequest, CreateInstrumentResponse,
    CreateMarketRequest, CreateMarketResponse,
    // Auth via gRPC metadata: authorization: Bearer <token>
    Market, Instrument, Order, OrderType, OrderStatus, MarketType,
};

pub use settlement::{
    settlement_service_client::SettlementServiceClient,
    GetPendingProposalsRequest, GetPendingProposalsResponse,
    GetSettlementStatusRequest, GetSettlementStatusResponse,
    CantonToServerMessage, ServerToCantonMessage,
    CantonNodeAuth, SettlementProposalMessage, PreconfirmationRequest, PreconfirmationDecision,
    SettlementStage, PreconfirmationResponse, PartyRole,
    // Preconfirmation and settlement status
    SubmitPreconfirmationRequest, NextAction, DvpStepStatus, DvpStepStatusEnum,
    // Disclosed contracts for cross-party settlement
    DisclosedContractMessage, SaveDisclosedContractRequest, SaveDisclosedContractResponse,
    GetDisclosedContractsRequest, GetDisclosedContractsResponse,
    // Record completed settlements
    RecordSettlementRequest, RecordSettlementResponse,
    // Transaction and settlement event recording
    RecordTransactionRequest, RecordTransactionResponse,
    RecordSettlementEventRequest, RecordSettlementEventResponse,
    // Auth for external parties via gRPC metadata: authorization: Bearer <token>
    TransactionType, TransactionResult, SenderType,
    SettlementEventType, SettlementEventResult, RecordedByRole,
    // Get settlement proposal by ID
    GetSettlementProposalByIdRequest, GetSettlementProposalByIdResponse,
};

pub use pricing::{
    pricing_service_client::PricingServiceClient,
    GetPriceRequest, GetPriceResponse,
    GetPricesRequest, GetPricesResponse,
    Price,
};

pub use ledger::{
    d_app_provider_service_client::DAppProviderServiceClient,
    GetActiveContractsRequest, GetActiveContractsResponse, ActiveContractInfo,
    GetBalancesRequest, GetBalancesResponse, TokenBalance,
    GetPreapprovalsRequest, GetPreapprovalsResponse, PreapprovalInfo,
    GetDsoRatesRequest, GetDsoRatesResponse,
    GetSettlementContractsRequest, GetSettlementContractsResponse, DiscoveredContract,
    GetServiceInfoRequest, GetServiceInfoResponse,
    PrepareTransactionRequest, PrepareTransactionResponse,
    ExecuteTransactionRequest, ExecuteTransactionResponse,
    TransactionOperation, TransactionStatus, TrafficEstimate,
    ProviderError, ProviderErrorCode,
    PayFeeParams, ProposeDvpParams, AcceptDvpParams, AllocateParams,
    TransferCcParams, RequestPreapprovalParams,
    RequestRecurringPrepaidParams, RequestRecurringPayasyougoParams,
    RequestUserServiceParams,
    // Onboarding
    OnboardingStatus,
    GetAgentConfigRequest, GetAgentConfigResponse,
    RegisterAgentRequest, RegisterAgentResponse,
    GetOnboardingStatusRequest, GetOnboardingStatusResponse,
    SubmitOnboardingSignatureRequest, SubmitOnboardingSignatureResponse,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orderbook_descriptor_pool() {
        // Verify orderbook descriptor pool is accessible
        let pool = &*ORDERBOOK_DESCRIPTOR_POOL;

        // Find a known message in the pool
        let market_descriptor = pool
            .get_message_by_name("silvana.orderbook.v1.Market")
            .expect("Should find Market message");

        assert_eq!(market_descriptor.full_name(), "silvana.orderbook.v1.Market");
    }

    #[test]
    fn test_settlement_descriptor_pool() {
        // Verify settlement descriptor pool is accessible
        let pool = &*SETTLEMENT_DESCRIPTOR_POOL;

        // Find a known message in the pool
        let proposal_descriptor = pool
            .get_message_by_name("silvana.settlement.v1.SettlementProposalMessage")
            .expect("Should find SettlementProposalMessage");

        assert_eq!(proposal_descriptor.full_name(), "silvana.settlement.v1.SettlementProposalMessage");
    }

    #[test]
    fn test_file_descriptor_sets() {
        assert!(!orderbook::FILE_DESCRIPTOR_SET.is_empty());
        assert!(!settlement::FILE_DESCRIPTOR_SET.is_empty());

        println!("Orderbook descriptor set: {} bytes", orderbook::FILE_DESCRIPTOR_SET.len());
        println!("Settlement descriptor set: {} bytes", settlement::FILE_DESCRIPTOR_SET.len());
    }
}
