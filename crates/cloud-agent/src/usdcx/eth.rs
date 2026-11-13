//! Ethereum side of the xReserve bridge: USDC deposits to Canton and the
//! release checks of withdrawals.
//!
//! Deposit = USDC `approve(xReserve, value)` + `xReserve.depositToRemote(value,
//! 10001, keccak256(party), USDC, 0, party bytes)`. A plain USDC transfer to the
//! xReserve address carries no Canton recipient and would be stuck.
//! Withdrawals are released automatically by a Circle relayer
//! (`GatewayMinter.gatewayMint` -> `AttestationUsed`); nothing is signed here for them.
//!
//! Transactions are filled and signed locally first ([`EthSigner::sign_call`]),
//! so the caller can persist the hash, nonce and raw bytes before anything is
//! broadcast, and later rebroadcast the same bytes ([`EthChain::broadcast_raw`]).

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use alloy::eips::eip2718::Encodable2718;
use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::client::RpcClient;
use alloy::rpc::types::{Filter, Log, TransactionReceipt, TransactionRequest};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::{SolCall, SolEvent};
use anyhow::{Context, Result, anyhow, bail, ensure};
use async_trait::async_trait;
use rust_decimal::Decimal;
use zeroize::Zeroizing;

use super::chain::{CANTON_DOMAIN, EthNetwork};
use super::ethaddr::{DepositIntent, addr, b256, deposit_nonce, party_hash, wei_to_eth};

/// ABI bindings. The 8-field events make clippy's argument count trip on generated code.
#[allow(clippy::too_many_arguments)]
mod abi {
    use alloy::sol;

    sol! {
        #[sol(rpc)]
        interface IERC20 {
            function approve(address spender, uint256 amount) external returns (bool);
            function allowance(address owner, address spender) external view returns (uint256);
            function balanceOf(address account) external view returns (uint256);
        }

        #[sol(rpc)]
        interface IXReserve {
            function depositToRemote(uint256 value, uint32 remoteDomain, bytes32 remoteRecipient, address localToken, uint256 maxFee, bytes calldata hookData) external;
            function paused() external view returns (bool);
            function domain() external view returns (uint32);
            function domainDepositsPaused(uint32 remoteDomain) external view returns (bool);
            function isRemoteDomainRegistered(uint32 remoteDomain) external view returns (bool);
            function getRemoteToken(uint32 remoteDomain, address localToken) external view returns (bytes32);
            function isBlocklisted(uint32 remoteDomain, bytes32 remoteRecipient) external view returns (bool);

            event DepositedToRemote(address indexed localToken, uint256 value, address indexed localDepositor, bytes32 indexed remoteRecipient, uint32 remoteDomain, bytes32 remoteToken, uint256 maxFee, bytes hookData);
        }

        interface IGatewayMinter {
            event AttestationUsed(address indexed token, address indexed recipient, bytes32 indexed transferSpecHash, uint32 sourceDomain, bytes32 sourceDepositor, bytes32 sourceSigner, uint256 value);
        }
    }
}
use abi::{IERC20, IGatewayMinter, IXReserve};

/// Longest wait for one Ethereum receipt. On timeout the tx is never resent.
pub const RECEIPT_TIMEOUT: Duration = Duration::from_secs(600);
/// Longest wait for one JSON-RPC request.
const RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Gas used for the max-cost estimate in the deposit summary (observed ~126k
/// + ~46k), before the signer's 20% margin.
const DEPOSIT_GAS: u64 = 150_000;
const APPROVE_GAS: u64 = 70_000;
/// eth_getLogs range per request; halved down to the minimum when a provider
/// refuses the range.
const LOG_CHUNK_BLOCKS: u64 = 5_000;
const MIN_LOG_CHUNK_BLOCKS: u64 = 100;
/// Receipt polling interval while waiting.
const RECEIPT_POLL: Duration = Duration::from_secs(4);
/// Gas estimates right after an approve may not see the new allowance yet.
const ESTIMATE_ATTEMPTS: u32 = 3;
const ESTIMATE_RETRY: Duration = Duration::from_secs(4);

pub fn approve_calldata(net: &EthNetwork, value: U256) -> Bytes {
    IERC20::approveCall {
        spender: addr(net.xreserve),
        amount: value,
    }
    .abi_encode()
    .into()
}

fn deposit_call(net: &EthNetwork, value: U256, party: &str) -> IXReserve::depositToRemoteCall {
    IXReserve::depositToRemoteCall {
        value,
        remoteDomain: CANTON_DOMAIN,
        remoteRecipient: party_hash(party),
        localToken: addr(net.usdc),
        maxFee: U256::ZERO,
        hookData: Bytes::copy_from_slice(party.as_bytes()),
    }
}

pub fn deposit_calldata(net: &EthNetwork, value: U256, party: &str) -> Bytes {
    deposit_call(net, value, party).abi_encode().into()
}

/// A `DepositedToRemote` event seen on Ethereum.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedDeposit {
    pub tx_hash: B256,
    pub block_number: u64,
    pub log_index: u64,
    pub intent: DepositIntent,
}

/// Decode a `DepositedToRemote` log (address, topics, data) into the deposit intent.
pub fn observed_from_log(
    net: &EthNetwork,
    tx_hash: B256,
    block_number: u64,
    log_index: u64,
    address: Address,
    topics: &[B256],
    data: &[u8],
) -> Result<ObservedDeposit> {
    ensure!(
        address == addr(net.xreserve),
        "log is not from the xReserve contract"
    );
    let ev = IXReserve::DepositedToRemote::decode_raw_log(topics.iter().copied(), data)
        .context("decode DepositedToRemote")?;
    Ok(ObservedDeposit {
        tx_hash,
        block_number,
        log_index,
        intent: DepositIntent {
            amount: ev.value,
            remote_domain: ev.remoteDomain,
            remote_token: ev.remoteToken,
            remote_recipient: ev.remoteRecipient,
            local_token: ev.localToken,
            local_depositor: ev.localDepositor,
            max_fee: ev.maxFee,
            nonce: deposit_nonce(tx_hash, log_index),
            hook_data: ev.hookData,
        },
    })
}

/// What a caller needs from a transaction receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptInfo {
    pub tx_hash: B256,
    pub block_number: Option<u64>,
    pub block_hash: Option<B256>,
    pub success: bool,
    /// The xReserve `DepositedToRemote` log, decoded, when the receipt has one.
    pub deposit: Option<ObservedDeposit>,
}

impl ReceiptInfo {
    /// The receipt's deposit; fails when the transaction reverted or is not an
    /// xReserve deposit.
    pub fn deposit(&self) -> Result<&ObservedDeposit> {
        ensure!(self.success, "Ethereum tx {} reverted", self.tx_hash);
        self.deposit.as_ref().ok_or_else(|| {
            anyhow!(
                "tx {} has no xReserve DepositedToRemote event — not an xReserve deposit",
                self.tx_hash
            )
        })
    }
}

fn receipt_info(net: &EthNetwork, receipt: &TransactionReceipt) -> Result<ReceiptInfo> {
    let deposit_log: Option<&Log> = receipt.inner.logs().iter().find(|l| {
        l.address() == addr(net.xreserve)
            && l.topics().first() == Some(&IXReserve::DepositedToRemote::SIGNATURE_HASH)
    });
    let deposit = match deposit_log {
        Some(log) => Some(observed_from_log(
            net,
            receipt.transaction_hash,
            receipt
                .block_number
                .ok_or_else(|| anyhow!("receipt without block number"))?,
            log.log_index
                .ok_or_else(|| anyhow!("receipt log without logIndex"))?,
            log.address(),
            log.topics(),
            &log.data().data,
        )?),
        None => None,
    };
    Ok(ReceiptInfo {
        tx_hash: receipt.transaction_hash,
        block_number: receipt.block_number,
        block_hash: receipt.block_hash,
        success: receipt.status(),
        deposit,
    })
}

/// One USDC release to a withdrawal destination (GatewayMinter `AttestationUsed`).
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub tx_hash: B256,
    pub log_index: u64,
    pub block_number: u64,
    pub transfer_spec_hash: B256,
    pub source_domain: u32,
    pub value: U256,
}

impl Release {
    /// `txHash:transferSpecHash`, lowercase: the release transaction (for
    /// links) and the attestation's `transferSpecHash` (the ownership key).
    pub fn key(&self) -> String {
        format!("{}:{}", self.tx_hash, self.transfer_spec_hash).to_ascii_lowercase()
    }

    /// What ownership is decided on: the attestation's `transferSpecHash`
    /// (one per release, unchanged when a reorg moves the log).
    pub fn owner_key(&self) -> String {
        self.transfer_spec_hash.to_string().to_ascii_lowercase()
    }
}

/// The ownership key of a [`Release::key`] entry: its `transferSpecHash`, or
/// the whole entry when it has no `:`.
pub fn release_owner_key(entry: &str) -> String {
    entry
        .rsplit_once(':')
        .map(|(_, spec)| spec)
        .unwrap_or(entry)
        .trim()
        .to_ascii_lowercase()
}

/// A signed transaction that has not been broadcast yet. Persist the hash,
/// nonce and raw bytes before broadcasting: the same bytes can be sent again
/// later with [`EthChain::broadcast_raw`] (hex) or
/// [`EthChain::broadcast_raw_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTx {
    pub hash: B256,
    pub nonce: u64,
    /// The EIP-2718 encoding sent by `eth_sendRawTransaction`.
    raw: Bytes,
}

impl SignedTx {
    pub fn new(hash: B256, nonce: u64, raw: Bytes) -> Self {
        Self { hash, nonce, raw }
    }

    /// The signed EIP-2718 bytes (contain no key material).
    pub fn raw(&self) -> &Bytes {
        &self.raw
    }
}

/// [`EthSigner::broadcast`] or [`EthChain::broadcast_raw_bytes`] failed because
/// the node refused the transaction outright: it was not sent and can never be mined.
#[derive(Debug, Clone)]
pub struct NotSent(pub String);

impl fmt::Display for NotSent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the Ethereum node refused the transaction: {}", self.0)
    }
}

impl std::error::Error for NotSent {}

/// True when `err` is (or wraps) a [`NotSent`].
pub fn is_not_sent(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.downcast_ref::<NotSent>().is_some())
}

/// JSON-RPC errors of `eth_sendRawTransaction` after which the transaction
/// certainly is not in any pool. Anything else may have been sent.
pub fn is_definitive_rejection(message: &str) -> bool {
    const REFUSED: [&str; 11] = [
        "insufficient funds",
        "intrinsic gas too low",
        "exceeds block gas limit",
        "max priority fee per gas higher than max fee per gas",
        "tip above fee cap",
        "invalid sender",
        "invalid signature",
        "transaction type not supported",
        "oversized data",
        "replacement transaction underpriced",
        "exceeds the configured cap",
    ];
    let m = message.to_ascii_lowercase();
    REFUSED.iter().any(|r| m.contains(r))
}

/// Signs with a local secp256k1 key. Transactions are filled and signed
/// locally so the caller can persist the hash and nonce BEFORE anything is
/// broadcast.
// `async_trait` marks its boxed futures `#[must_use]` again.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait EthSigner: Send + Sync {
    fn from(&self) -> Address;
    /// Fill (pending nonce, gas estimate + 20%, EIP-1559 fees) and sign a call.
    /// Fails with nothing broadcast when the call would revert.
    async fn sign_call(&self, to: Address, input: Bytes) -> Result<SignedTx>;
    /// Broadcast a signed transaction. An error does NOT prove it was not
    /// sent, unless it is a [`NotSent`].
    async fn broadcast(&self, tx: SignedTx) -> Result<()>;
}

/// Read access to Ethereum, pinned to one network's chain, plus the
/// rebroadcast of already-signed transactions.
// `async_trait` marks its boxed futures `#[must_use]` again.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait EthChain: Send + Sync {
    async fn block_number(&self) -> Result<u64>;
    async fn receipt(&self, tx_hash: B256) -> Result<Option<ReceiptInfo>>;
    /// Waits for one confirmation, bounded by `timeout`.
    async fn wait_receipt(&self, tx_hash: B256, timeout: Duration) -> Result<ReceiptInfo>;
    /// Whether the node knows the transaction (mined or in its mempool).
    /// [`EthChain::receipt`] tells the two apart.
    async fn tx_known(&self, tx_hash: B256) -> Result<bool>;
    /// Next nonce of `owner` counting mined transactions only (`latest`).
    async fn mined_nonce(&self, owner: Address) -> Result<u64>;
    /// Next nonce of `owner` counting the node's mempool too (`pending`).
    async fn pending_nonce(&self, owner: Address) -> Result<u64>;
    async fn eth_balance(&self, owner: Address) -> Result<U256>;
    /// Rough upper bound of the gas cost (wei, ETH) of approve (if needed) + deposit.
    async fn max_gas_cost(&self, needs_approve: bool) -> Result<(U256, Decimal)>;
    async fn usdc_balance(&self, owner: Address) -> Result<U256>;
    /// USDC allowance of `owner` for the xReserve contract.
    async fn usdc_allowance(&self, owner: Address) -> Result<U256>;
    /// xReserve state that would make a deposit revert or strand; returns
    /// warnings for checks that could not be made.
    async fn preflight(&self, party: &str) -> Result<Vec<String>>;
    /// xReserve deposits from `depositor` to `party` since `from_block`.
    async fn find_deposits(
        &self,
        depositor: Address,
        party: &str,
        from_block: u64,
    ) -> Result<Vec<ObservedDeposit>>;
    /// Canton-sourced USDC releases to `recipient` from `from_block` on, oldest
    /// first. One burn can be split across several releases.
    async fn find_releases(&self, recipient: Address, from_block: u64) -> Result<Vec<Release>>;
    /// Sends already-signed EIP-2718 bytes (e.g. a persisted [`SignedTx::raw`])
    /// and returns the hash the node reports. An error does NOT prove the
    /// transaction is not in a pool, unless it is a [`NotSent`]; a node that
    /// already has it answers with an error such as "already known".
    async fn broadcast_raw_bytes(&self, raw: &[u8]) -> Result<B256>;
    /// [`EthChain::broadcast_raw_bytes`] for the hex form of the signed bytes
    /// (`hex0x(signed.raw())`, as persisted before the first broadcast; the
    /// `0x` is optional).
    async fn broadcast_raw(&self, raw_hex: &str) -> Result<B256> {
        let text = raw_hex.trim();
        let text = text.strip_prefix("0x").unwrap_or(text);
        let raw = hex::decode(text).map_err(|_| anyhow!("the signed transaction is not hex"))?;
        ensure!(!raw.is_empty(), "the signed transaction is empty");
        self.broadcast_raw_bytes(&raw).await
    }
    /// A signer for a hex secp256k1 private key (64 hex characters, with or
    /// without `0x`). The key is never logged, kept as text or put in an error.
    async fn signer(&self, key: &str) -> Result<Box<dyn EthSigner>>;
}

fn rpc_url(raw: &str) -> Result<reqwest::Url> {
    raw.trim()
        .parse()
        .map_err(|_| anyhow!("the Ethereum RPC URL is not a valid URL"))
}

/// A log's transaction, block and index; all three are required.
fn log_position(log: &Log) -> Result<(B256, u64, u64)> {
    Ok((
        log.transaction_hash
            .ok_or_else(|| anyhow!("log without transactionHash"))?,
        log.block_number
            .ok_or_else(|| anyhow!("log without blockNumber"))?,
        log.log_index
            .ok_or_else(|| anyhow!("log without logIndex"))?,
    ))
}

/// Gas with the signer's margin: estimate + 20%.
fn with_margin(gas: u64) -> u64 {
    gas.saturating_add(gas / 5)
}

/// [`EthChain`] over an HTTP JSON-RPC provider.
pub struct AlloyChain {
    provider: DynProvider,
    net: &'static EthNetwork,
}

impl AlloyChain {
    /// Connects and fails unless the pins are valid, the RPC is on the
    /// network's chain and the xReserve contract reports domain 0 (Ethereum).
    pub async fn connect(net: &'static EthNetwork, rpc: &str) -> Result<Self> {
        net.check_pins()?;
        let http = reqwest::Client::builder()
            .timeout(RPC_REQUEST_TIMEOUT)
            .build()
            .context("Ethereum RPC client")?;
        let client = RpcClient::new_http_with_client(http, rpc_url(rpc)?);
        let provider = ProviderBuilder::new().connect_client(client).erased();
        let chain_id = provider.get_chain_id().await.context("eth_chainId")?;
        ensure!(
            chain_id == net.eth_chain_id,
            "the Ethereum RPC endpoint is on chain {chain_id}, expected {} ({}). The xReserve \
             address also exists on other chains, so nothing is sent.",
            net.eth_chain_id,
            net.eth_chain_name
        );
        let domain = IXReserve::new(addr(net.xreserve), &provider)
            .domain()
            .call()
            .await
            .context("xReserve.domain()")?;
        ensure!(
            domain == 0,
            "xReserve.domain() is {domain}, expected 0 (Ethereum)"
        );
        Ok(Self { provider, net })
    }

    /// The network this chain is pinned to.
    pub fn network(&self) -> &'static EthNetwork {
        self.net
    }
}

#[async_trait]
impl EthChain for AlloyChain {
    async fn block_number(&self) -> Result<u64> {
        self.provider
            .get_block_number()
            .await
            .context("eth_blockNumber")
    }

    async fn receipt(&self, tx_hash: B256) -> Result<Option<ReceiptInfo>> {
        let receipt = self
            .provider
            .get_transaction_receipt(tx_hash)
            .await
            .context("eth_getTransactionReceipt")?;
        receipt.map(|r| receipt_info(self.net, &r)).transpose()
    }

    /// RPC errors are retried until the deadline; a receipt that cannot be
    /// decoded is returned as an error at once.
    async fn wait_receipt(&self, tx_hash: B256, timeout: Duration) -> Result<ReceiptInfo> {
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow!("receipt timeout of {}s is too long", timeout.as_secs()))?;
        let mut last_error: Option<String> = None;
        loop {
            let lookup =
                tokio::time::timeout_at(deadline, self.provider.get_transaction_receipt(tx_hash))
                    .await;
            match lookup {
                Ok(Ok(Some(r))) => return receipt_info(self.net, &r),
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(e)) => {
                    let message = e.to_string();
                    if last_error.as_deref() != Some(message.as_str()) {
                        tracing::warn!(tx = %tx_hash, error = %message, "receipt lookup failed; retrying");
                        last_error = Some(message);
                    }
                }
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                bail!(
                    "no receipt for {tx_hash} within {}s{}",
                    timeout.as_secs(),
                    last_error
                        .map(|e| format!(" (last RPC error: {e})"))
                        .unwrap_or_default()
                );
            }
            let wake = now
                .checked_add(RECEIPT_POLL)
                .map_or(deadline, |t| t.min(deadline));
            tokio::time::sleep_until(wake).await;
        }
    }

    async fn tx_known(&self, tx_hash: B256) -> Result<bool> {
        Ok(self
            .provider
            .get_transaction_by_hash(tx_hash)
            .await
            .context("eth_getTransactionByHash")?
            .is_some())
    }

    async fn mined_nonce(&self, owner: Address) -> Result<u64> {
        self.provider
            .get_transaction_count(owner)
            .latest()
            .await
            .context("eth_getTransactionCount")
    }

    async fn pending_nonce(&self, owner: Address) -> Result<u64> {
        self.provider
            .get_transaction_count(owner)
            .pending()
            .await
            .context("eth_getTransactionCount")
    }

    async fn eth_balance(&self, owner: Address) -> Result<U256> {
        self.provider
            .get_balance(owner)
            .await
            .context("eth_getBalance")
    }

    async fn max_gas_cost(&self, needs_approve: bool) -> Result<(U256, Decimal)> {
        let fees = self
            .provider
            .estimate_eip1559_fees()
            .await
            .context("estimate EIP-1559 fees")?;
        let gas = if needs_approve {
            DEPOSIT_GAS.saturating_add(APPROVE_GAS)
        } else {
            DEPOSIT_GAS
        };
        let wei = U256::from(with_margin(gas))
            .checked_mul(U256::from(fees.max_fee_per_gas))
            .ok_or_else(|| anyhow!("gas cost is out of range"))?;
        Ok((wei, wei_to_eth(wei)))
    }

    async fn usdc_balance(&self, owner: Address) -> Result<U256> {
        IERC20::new(addr(self.net.usdc), &self.provider)
            .balanceOf(owner)
            .call()
            .await
            .context("USDC.balanceOf")
    }

    async fn usdc_allowance(&self, owner: Address) -> Result<U256> {
        IERC20::new(addr(self.net.usdc), &self.provider)
            .allowance(owner, addr(self.net.xreserve))
            .call()
            .await
            .context("USDC.allowance")
    }

    async fn preflight(&self, party: &str) -> Result<Vec<String>> {
        let net = self.net;
        let x = IXReserve::new(addr(net.xreserve), &self.provider);
        ensure!(
            !x.paused().call().await.context("xReserve.paused()")?,
            "xReserve is paused"
        );
        ensure!(
            !x.domainDepositsPaused(CANTON_DOMAIN)
                .call()
                .await
                .context("xReserve.domainDepositsPaused")?,
            "xReserve deposits to Canton (domain {CANTON_DOMAIN}) are paused"
        );
        ensure!(
            x.isRemoteDomainRegistered(CANTON_DOMAIN)
                .call()
                .await
                .context("xReserve.isRemoteDomainRegistered")?,
            "Canton (domain {CANTON_DOMAIN}) is not registered on xReserve"
        );
        let remote_token = x
            .getRemoteToken(CANTON_DOMAIN, addr(net.usdc))
            .call()
            .await
            .context("xReserve.getRemoteToken")?;
        ensure!(
            remote_token == b256(net.canton_remote_token),
            "xReserve maps USDC to {remote_token} on Canton, expected {}",
            net.canton_remote_token
        );
        let mut warnings = Vec::new();
        match x
            .isBlocklisted(CANTON_DOMAIN, party_hash(party))
            .call()
            .await
        {
            Ok(true) => bail!("the Canton recipient {party} is blocklisted on xReserve"),
            Ok(false) => {}
            Err(e) => warnings.push(format!("could not check the xReserve blocklist: {e}")),
        }
        Ok(warnings)
    }

    async fn find_deposits(
        &self,
        depositor: Address,
        party: &str,
        from_block: u64,
    ) -> Result<Vec<ObservedDeposit>> {
        let net = self.net;
        let latest = self.block_number().await?;
        let filter = Filter::new()
            .address(addr(net.xreserve))
            .event_signature(IXReserve::DepositedToRemote::SIGNATURE_HASH)
            .topic1(addr(net.usdc).into_word())
            .topic2(depositor.into_word())
            .topic3(party_hash(party));
        let mut out = Vec::new();
        for log in self
            .logs(&filter, from_block, latest, "DepositedToRemote")
            .await?
        {
            let (tx_hash, block_number, log_index) = log_position(&log)?;
            out.push(observed_from_log(
                net,
                tx_hash,
                block_number,
                log_index,
                log.address(),
                log.topics(),
                &log.data().data,
            )?);
        }
        Ok(out)
    }

    async fn find_releases(&self, recipient: Address, from_block: u64) -> Result<Vec<Release>> {
        let net = self.net;
        let latest = self.block_number().await?;
        let canton_depositor = addr(net.canton_rdd).into_word();
        let filter = Filter::new()
            .address(addr(net.gateway_minter))
            .event_signature(IGatewayMinter::AttestationUsed::SIGNATURE_HASH)
            .topic1(addr(net.usdc).into_word())
            .topic2(recipient.into_word());
        let mut out = Vec::new();
        for log in self
            .logs(&filter, from_block, latest, "AttestationUsed")
            .await?
        {
            let ev = log
                .log_decode::<IGatewayMinter::AttestationUsed>()
                .context("decode AttestationUsed")?;
            let data = &ev.inner.data;
            if data.sourceDepositor == canton_depositor {
                let (tx_hash, block_number, log_index) = log_position(&log)?;
                out.push(Release {
                    tx_hash,
                    log_index,
                    block_number,
                    transfer_spec_hash: data.transferSpecHash,
                    source_domain: data.sourceDomain,
                    value: data.value,
                });
            }
        }
        Ok(out)
    }

    async fn broadcast_raw_bytes(&self, raw: &[u8]) -> Result<B256> {
        send_raw(&self.provider, raw).await
    }

    async fn signer(&self, key: &str) -> Result<Box<dyn EthSigner>> {
        let signer = parse_private_key(key)?;
        let from = signer.address();
        Ok(Box::new(AlloySigner {
            provider: self.provider.clone(),
            wallet: EthereumWallet::from(signer),
            from,
            chain_id: self.net.eth_chain_id,
            last_nonce: Mutex::new(None),
        }))
    }
}

impl AlloyChain {
    /// `eth_getLogs` for `filter` over `from..=to`, oldest first, in chunks
    /// that are halved (down to [`MIN_LOG_CHUNK_BLOCKS`]) whenever a request
    /// fails, e.g. on providers that cap the block range. Removed logs are
    /// skipped.
    async fn logs(&self, filter: &Filter, from: u64, to: u64, what: &str) -> Result<Vec<Log>> {
        let mut out = Vec::new();
        let mut chunk = LOG_CHUNK_BLOCKS;
        let mut start = from;
        while start <= to {
            let end = start.saturating_add(chunk.saturating_sub(1)).min(to);
            let ranged = filter.clone().from_block(start).to_block(end);
            match self.provider.get_logs(&ranged).await {
                Ok(logs) => {
                    out.extend(logs.into_iter().filter(|l| !l.removed));
                    match end.checked_add(1) {
                        Some(next) => start = next,
                        None => break,
                    }
                }
                Err(e) if chunk > MIN_LOG_CHUNK_BLOCKS => {
                    tracing::debug!(error = %e, chunk, "eth_getLogs {what} failed; halving the range");
                    chunk = (chunk / 2).max(MIN_LOG_CHUNK_BLOCKS);
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e))
                        .with_context(|| format!("eth_getLogs {what} {start}..{end}"));
                }
            }
        }
        Ok(out)
    }
}

/// `eth_sendRawTransaction`; the receipt is polled separately
/// ([`EthChain::wait_receipt`]). Definitive refusals become [`NotSent`].
async fn send_raw(provider: &DynProvider, raw: &[u8]) -> Result<B256> {
    match provider.send_raw_transaction(raw).await {
        Ok(pending) => Ok(*pending.tx_hash()),
        Err(e) => {
            if let Some(resp) = e.as_error_resp()
                && is_definitive_rejection(&resp.message)
            {
                return Err(anyhow::Error::new(NotSent(format!(
                    "{} (code {})",
                    resp.message, resp.code
                ))));
            }
            Err(anyhow::Error::new(e)).context("eth_sendRawTransaction")
        }
    }
}

/// A local signer from a hex private key: exactly 32 bytes as 64 hex
/// characters, with or without `0x`, surrounding whitespace ignored. The
/// decoded bytes are wiped on drop and the input never appears in an error.
fn parse_private_key(key: &str) -> Result<PrivateKeySigner> {
    let text = key.trim();
    let text = text.strip_prefix("0x").unwrap_or(text);
    let mut bytes = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(text, bytes.as_mut_slice()).map_err(|_| {
        anyhow!("invalid Ethereum private key: expected 32 bytes as 64 hex characters")
    })?;
    PrivateKeySigner::from_slice(bytes.as_slice())
        .map_err(|_| anyhow!("invalid Ethereum private key: not a valid secp256k1 secret key"))
}

struct AlloySigner {
    provider: DynProvider,
    wallet: EthereumWallet,
    from: Address,
    chain_id: u64,
    /// The nonce of the last transaction signed here: the next one is at
    /// least one higher, even when a lagging RPC node reports otherwise.
    last_nonce: Mutex<Option<u64>>,
}

/// Never prints key material: only the address and chain.
impl fmt::Debug for AlloySigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AlloySigner")
            .field("from", &self.from)
            .field("chain_id", &self.chain_id)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl EthSigner for AlloySigner {
    fn from(&self) -> Address {
        self.from
    }

    async fn sign_call(&self, to: Address, input: Bytes) -> Result<SignedTx> {
        ensure!(
            to != Address::ZERO,
            "refusing to sign a call to the zero address"
        );
        let pending = self
            .provider
            .get_transaction_count(self.from)
            .pending()
            .await
            .context("eth_getTransactionCount")?;
        let last = *self
            .last_nonce
            .lock()
            .map_err(|_| anyhow!("nonce lock poisoned"))?;
        let nonce = match last {
            Some(n) => pending.max(n.checked_add(1).ok_or_else(|| anyhow!("nonce overflow"))?),
            None => pending,
        };
        let request = TransactionRequest::default()
            .with_from(self.from)
            .with_to(to)
            .with_input(input)
            .with_nonce(nonce)
            .with_chain_id(self.chain_id);
        let mut attempt: u32 = 1;
        let gas = loop {
            match self.provider.estimate_gas(request.clone()).await {
                Ok(gas) => break gas,
                Err(e) if attempt < ESTIMATE_ATTEMPTS => {
                    tracing::debug!(error = %e, attempt, "eth_estimateGas failed; retrying");
                    attempt = attempt.saturating_add(1);
                    tokio::time::sleep(ESTIMATE_RETRY).await;
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e))
                        .context("eth_estimateGas (the call would revert)");
                }
            }
        };
        let fees = self
            .provider
            .estimate_eip1559_fees()
            .await
            .context("estimate EIP-1559 fees")?;
        let envelope = request
            .with_gas_limit(with_margin(gas))
            .with_max_fee_per_gas(fees.max_fee_per_gas)
            .with_max_priority_fee_per_gas(fees.max_priority_fee_per_gas)
            .build(&self.wallet)
            .await
            .context("sign transaction")?;
        *self
            .last_nonce
            .lock()
            .map_err(|_| anyhow!("nonce lock poisoned"))? = Some(nonce);
        Ok(SignedTx::new(
            *envelope.tx_hash(),
            nonce,
            envelope.encoded_2718().into(),
        ))
    }

    async fn broadcast(&self, tx: SignedTx) -> Result<()> {
        send_raw(&self.provider, tx.raw()).await.map(|_| ())
    }
}

#[cfg(test)]
impl AlloyChain {
    /// Wraps a provider without the connect-time chain checks.
    fn from_provider(net: &'static EthNetwork, provider: DynProvider) -> Self {
        Self { provider, net }
    }
}

#[cfg(test)]
mod tests {
    use super::super::chain::MAINNET;
    use super::super::ethaddr::hex0x;
    use super::*;
    use alloy::consensus::TxEnvelope;
    use alloy::eips::eip2718::Decodable2718;
    use alloy::primitives::{U64, keccak256};
    use alloy::rpc::types::FeeHistory;
    use alloy::transports::mock::Asserter;
    use serde_json::Value;

    // ---- A synthetic deposit vector (made-up party, depositor and tx). ----
    //
    // The expected values were produced once by an independent implementation
    // whose encoding matches a Circle-attested MainNet deposit (payload and
    // messageHash); LOG_DATA was also encoded by hand, not with `sol!`.

    const PARTY: &str =
        "vector-party::12200123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const AMOUNT_UNITS: u64 = 12_345_678;
    const DEPOSITOR: Address = Address::repeat_byte(0x11);
    const TX_HASH: B256 = B256::repeat_byte(0x22);
    const BLOCK_NUMBER: u64 = 0x0123_4567;
    const LOG_INDEX: u64 = 42;

    const PARTY_HASH: &str = "0x361d208c40f91c3081102a6dacb944e3bae2400f41ca1f42e1b3dedf7615838b";
    const DEPOSIT_CALLDATA: &str = concat!(
        "0xfaadb53b",
        "0000000000000000000000000000000000000000000000000000000000bc614e",
        "0000000000000000000000000000000000000000000000000000000000002711",
        "361d208c40f91c3081102a6dacb944e3bae2400f41ca1f42e1b3dedf7615838b",
        "000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "00000000000000000000000000000000000000000000000000000000000000c0",
        "0000000000000000000000000000000000000000000000000000000000000052",
        "766563746f722d70617274793a3a313232303031323334353637383961626364",
        "6566303132333435363738396162636465663031323334353637383961626364",
        "6566303132333435363738396162636465660000000000000000000000000000",
    );
    const LOG_DATA: &str = concat!(
        "0x",
        "0000000000000000000000000000000000000000000000000000000000bc614e",
        "0000000000000000000000000000000000000000000000000000000000002711",
        "661237037dc811823d8b2de17aaabb8ef2ac9b713ca7db3b01fc7f7baf7db562",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "00000000000000000000000000000000000000000000000000000000000000a0",
        "0000000000000000000000000000000000000000000000000000000000000052",
        "766563746f722d70617274793a3a313232303031323334353637383961626364",
        "6566303132333435363738396162636465663031323334353637383961626364",
        "6566303132333435363738396162636465660000000000000000000000000000",
    );
    const DEPOSIT_NONCE: &str =
        "0xd9d6b0ac57f4692740c8cb3c254600cd03d4182b442d947eaa4bd13d02a42e6b";
    const PAYLOAD: &str = concat!(
        "0x5a2e0acd",                                                       // magic
        "00000001",                                                         // version
        "0000000000000000000000000000000000000000000000000000000000bc614e", // amount
        "00002711",                                                         // remoteDomain
        "661237037dc811823d8b2de17aaabb8ef2ac9b713ca7db3b01fc7f7baf7db562", // remoteToken
        "361d208c40f91c3081102a6dacb944e3bae2400f41ca1f42e1b3dedf7615838b", // remoteRecipient
        "000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", // localToken
        "0000000000000000000000001111111111111111111111111111111111111111", // localDepositor
        "0000000000000000000000000000000000000000000000000000000000000000", // maxFee
        "d9d6b0ac57f4692740c8cb3c254600cd03d4182b442d947eaa4bd13d02a42e6b", // nonce
        "00000052",                                                         // hookDataLen
        "766563746f722d70617274793a3a313232303031323334353637383961626364",
        "6566303132333435363738396162636465663031323334353637383961626364",
        "656630313233343536373839616263646566",
    );
    const MESSAGE_HASH: &str = "0xbde3500a47e49de52c100138dbdd29ad21072a5921e5b5216f3884514bbdb93b";

    fn deposit_event() -> IXReserve::DepositedToRemote {
        IXReserve::DepositedToRemote {
            localToken: addr(MAINNET.usdc),
            value: U256::from(AMOUNT_UNITS),
            localDepositor: DEPOSITOR,
            remoteRecipient: party_hash(PARTY),
            remoteDomain: CANTON_DOMAIN,
            remoteToken: b256(MAINNET.canton_remote_token),
            maxFee: U256::ZERO,
            hookData: Bytes::copy_from_slice(PARTY.as_bytes()),
        }
    }

    fn rpc_log(address: Address, data: alloy::primitives::LogData, log_index: u64) -> Log {
        Log {
            inner: alloy::primitives::Log { address, data },
            block_hash: Some(B256::repeat_byte(0x33)),
            block_number: Some(BLOCK_NUMBER),
            block_timestamp: None,
            transaction_hash: Some(TX_HASH),
            transaction_index: Some(3),
            log_index: Some(log_index),
            removed: false,
        }
    }

    fn observed() -> ObservedDeposit {
        let log = deposit_event().encode_log_data();
        observed_from_log(
            &MAINNET,
            TX_HASH,
            BLOCK_NUMBER,
            LOG_INDEX,
            addr(MAINNET.xreserve),
            log.topics(),
            &log.data,
        )
        .unwrap()
    }

    #[test]
    fn vector_calldata_and_party_hash() {
        assert_eq!(hex0x(party_hash(PARTY).as_slice()), PARTY_HASH);
        let calldata = deposit_calldata(&MAINNET, U256::from(AMOUNT_UNITS), PARTY);
        assert_eq!(hex0x(&calldata), DEPOSIT_CALLDATA);
        assert_eq!(&calldata[..4], &[0xfa, 0xad, 0xb5, 0x3b]);
    }

    #[test]
    fn vector_log_decodes_to_the_intent() {
        let log = deposit_event().encode_log_data();
        assert_eq!(hex0x(&log.data), LOG_DATA);
        assert_eq!(
            log.topics(),
            &[
                IXReserve::DepositedToRemote::SIGNATURE_HASH,
                addr(MAINNET.usdc).into_word(),
                DEPOSITOR.into_word(),
                party_hash(PARTY),
            ]
        );
        let obs = observed();
        assert_eq!(obs.tx_hash, TX_HASH);
        assert_eq!(obs.block_number, BLOCK_NUMBER);
        assert_eq!(obs.log_index, LOG_INDEX);
        assert_eq!(obs.intent.amount, U256::from(AMOUNT_UNITS));
        assert_eq!(obs.intent.local_depositor, DEPOSITOR);
        assert_eq!(obs.intent.party().as_deref(), Some(PARTY));
        assert_eq!(obs.intent.remote_recipient, party_hash(PARTY));
        obs.intent.verify_for(&MAINNET, PARTY).unwrap();
        // Only the xReserve contract's logs are deposits.
        assert!(
            observed_from_log(
                &MAINNET,
                TX_HASH,
                BLOCK_NUMBER,
                LOG_INDEX,
                addr(MAINNET.usdc),
                log.topics(),
                &log.data,
            )
            .is_err()
        );
    }

    #[test]
    fn vector_nonce_payload_and_message_hash() {
        let obs = observed();
        assert_eq!(hex0x(obs.intent.nonce.as_slice()), DEPOSIT_NONCE);
        let payload = obs.intent.payload();
        assert_eq!(hex0x(&payload), PAYLOAD);
        assert_eq!(hex0x(obs.intent.message_hash().as_slice()), MESSAGE_HASH);
        let decoded = DepositIntent::decode(&hex::decode(&PAYLOAD[2..]).unwrap()).unwrap();
        assert_eq!(decoded, obs.intent);
    }

    #[test]
    fn approve_calldata_shape() {
        let data = approve_calldata(&MAINNET, U256::from(5_000_000u64));
        assert_eq!(&data[..4], &[0x09, 0x5e, 0xa7, 0xb3]);
        assert_eq!(data.len(), 4 + 64);
        assert_eq!(&data[16..36], addr(MAINNET.xreserve).as_slice());
    }

    #[test]
    fn release_key_is_the_transfer_spec_hash() {
        let r = Release {
            tx_hash: B256::repeat_byte(0xab),
            log_index: 7,
            block_number: 1,
            transfer_spec_hash: B256::repeat_byte(0xcd),
            source_domain: 10001,
            value: U256::from(1u64),
        };
        assert_eq!(
            r.key(),
            format!("0x{}:0x{}", "ab".repeat(32), "cd".repeat(32))
        );
        assert_eq!(r.owner_key(), format!("0x{}", "cd".repeat(32)));
        // A reorg that moves the log keeps its owner key.
        let moved = Release {
            log_index: 9,
            block_number: 2,
            ..r.clone()
        };
        assert_eq!(moved.owner_key(), r.owner_key());
        assert_eq!(release_owner_key(&r.key()), r.owner_key());
        assert_eq!(release_owner_key(" 0xAB "), "0xab");
    }

    #[test]
    fn broadcast_rejections_that_send_nothing() {
        for m in [
            "insufficient funds for gas * price + value: balance 1, tx cost 2",
            "intrinsic gas too low",
            "replacement transaction underpriced",
            "tx fee (1.10 ether) exceeds the configured cap (1.00 ether)",
            "max priority fee per gas higher than max fee per gas",
        ] {
            assert!(is_definitive_rejection(m), "{m}");
        }
        for m in [
            "nonce too low",
            "already known",
            "timeout",
            "internal error",
        ] {
            assert!(!is_definitive_rejection(m), "{m}");
        }
        let e = anyhow::Error::new(NotSent("insufficient funds".into())).context("deposit");
        assert!(is_not_sent(&e));
        assert!(!is_not_sent(&anyhow!(
            "eth_sendRawTransaction: connection reset"
        )));
    }

    #[test]
    fn gas_margin_saturates() {
        assert_eq!(with_margin(100_000), 120_000);
        assert_eq!(with_margin(u64::MAX), u64::MAX);
    }

    // ---- Local key handling. ----

    /// A throwaway key derived at run time (no key literal in the source).
    fn test_key() -> String {
        hex::encode(keccak256(b"usdcx test signer"))
    }

    #[test]
    fn private_key_forms_and_errors() {
        let key = test_key();
        let reference: PrivateKeySigner = key.parse().unwrap();
        for form in [key.clone(), format!("0x{key}"), format!("  0x{key}\n")] {
            assert_eq!(
                parse_private_key(&form).unwrap().address(),
                reference.address()
            );
        }
        let bad = [
            key[..62].to_string(),       // 31 bytes: never zero-padded
            format!("{}zz", &key[..62]), // not hex
            format!("{key}00"),          // 33 bytes
            "0".repeat(64),              // zero is not a secret key
            String::new(),
        ];
        for input in bad {
            let err = parse_private_key(&input).unwrap_err();
            let text = format!("{err:#} {err:?}");
            assert!(text.contains("invalid Ethereum private key"), "{text}");
            assert!(!text.contains(&key[..16]), "the key leaked into: {text}");
        }
    }

    // ---- RPC behaviour against a scripted JSON-RPC transport. ----

    fn mocked() -> (AlloyChain, Asserter) {
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new()
            .connect_mocked_client(asserter.clone())
            .erased();
        (AlloyChain::from_provider(&MAINNET, provider), asserter)
    }

    fn fee_history() -> FeeHistory {
        FeeHistory {
            oldest_block: 100,
            base_fee_per_gas: vec![10_000_000_000; 11],
            gas_used_ratio: vec![0.5; 10],
            reward: Some(vec![vec![1_000_000_000]; 10]),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn nonces_and_unknown_transactions() {
        let (chain, asserter) = mocked();
        asserter.push_success(&U64::from(7));
        asserter.push_success(&U64::from(9));
        asserter.push_success(&Value::Null);
        asserter.push_success(&Value::Null);
        assert_eq!(chain.mined_nonce(DEPOSITOR).await.unwrap(), 7);
        assert_eq!(chain.pending_nonce(DEPOSITOR).await.unwrap(), 9);
        assert!(!chain.tx_known(TX_HASH).await.unwrap());
        assert!(chain.receipt(TX_HASH).await.unwrap().is_none());
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn rebroadcast_classifies_node_errors() {
        let (chain, asserter) = mocked();
        let hash = B256::repeat_byte(0x44);
        asserter.push_success(&hash);
        assert_eq!(
            chain.broadcast_raw_bytes(&[0x02, 0x01]).await.unwrap(),
            hash
        );
        asserter.push_success(&hash);
        assert_eq!(chain.broadcast_raw(" 0x0201\n").await.unwrap(), hash);
        asserter.push_success(&hash);
        assert_eq!(chain.broadcast_raw("0201").await.unwrap(), hash);

        asserter.push_failure_msg("insufficient funds for gas * price + value");
        let refused = chain.broadcast_raw("0x02").await.unwrap_err();
        assert!(is_not_sent(&refused), "{refused:#}");

        asserter.push_failure_msg("already known");
        let unclear = chain.broadcast_raw_bytes(&[0x02]).await.unwrap_err();
        assert!(!is_not_sent(&unclear), "{unclear:#}");

        // Malformed input never reaches the node.
        assert!(chain.broadcast_raw("0xzz").await.is_err());
        assert!(chain.broadcast_raw("0x").await.is_err());
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn signs_locally_with_monotonic_nonces() {
        let (chain, asserter) = mocked();
        let signer = chain.signer(&test_key()).await.unwrap();
        let expected_from: PrivateKeySigner = test_key().parse().unwrap();
        assert_eq!(signer.from(), expected_from.address());
        let input = deposit_calldata(&MAINNET, U256::from(AMOUNT_UNITS), PARTY);

        // pending nonce, gas estimate, fee history
        asserter.push_success(&U64::from(7));
        asserter.push_success(&U64::from(100_000));
        asserter.push_success(&fee_history());
        let first = signer
            .sign_call(addr(MAINNET.xreserve), input.clone())
            .await
            .unwrap();
        assert_eq!(first.nonce, 7);
        assert_eq!(first.hash, keccak256(first.raw()));

        let envelope = TxEnvelope::decode_2718(&mut first.raw().as_ref()).unwrap();
        assert_eq!(*envelope.tx_hash(), first.hash);
        let TxEnvelope::Eip1559(signed) = &envelope else {
            panic!("expected an EIP-1559 transaction, got {envelope:?}");
        };
        let tx = signed.tx();
        assert_eq!(tx.chain_id, MAINNET.eth_chain_id);
        assert_eq!(tx.nonce, 7);
        assert_eq!(tx.gas_limit, 120_000);
        assert_eq!(
            tx.to,
            alloy::primitives::TxKind::Call(addr(MAINNET.xreserve))
        );
        assert_eq!(tx.input, input);
        assert_eq!(tx.value, U256::ZERO);
        assert!(tx.max_fee_per_gas >= tx.max_priority_fee_per_gas);
        assert!(tx.max_priority_fee_per_gas > 0);

        // A lagging node still reports 7: the next transaction takes 8.
        asserter.push_success(&U64::from(7));
        asserter.push_success(&U64::from(100_000));
        asserter.push_success(&fee_history());
        let second = signer
            .sign_call(addr(MAINNET.xreserve), input)
            .await
            .unwrap();
        assert_eq!(second.nonce, 8);
        assert_ne!(second.hash, first.hash);

        asserter.push_success(&first.hash);
        signer.broadcast(first.clone()).await.unwrap();
        asserter.push_failure_msg("nonce too low");
        let unclear = signer.broadcast(first).await.unwrap_err();
        assert!(!is_not_sent(&unclear));
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn sign_call_refuses_the_zero_address() {
        let (chain, asserter) = mocked();
        let signer = chain.signer(&test_key()).await.unwrap();
        let err = signer
            .sign_call(addr("not a pin"), Bytes::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("zero address"), "{err:#}");
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn sign_call_fails_when_the_call_would_revert() {
        let (chain, asserter) = mocked();
        let signer = chain.signer(&test_key()).await.unwrap();
        asserter.push_success(&U64::from(0));
        for _ in 0..ESTIMATE_ATTEMPTS {
            asserter.push_failure_msg("execution reverted");
        }
        let err = signer
            .sign_call(addr(MAINNET.xreserve), Bytes::new())
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("would revert"), "{err:#}");
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_receipt_gives_up_at_the_deadline() {
        let (chain, asserter) = mocked();
        for _ in 0..3 {
            asserter.push_success(&Value::Null);
        }
        // Then the scripted transport runs dry and every lookup errors.
        let err = chain
            .wait_receipt(TX_HASH, Duration::from_secs(20))
            .await
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("no receipt for"), "{text}");
        assert!(text.contains("within 20s"), "{text}");
        assert!(text.contains("last RPC error"), "{text}");
        assert!(
            chain
                .wait_receipt(TX_HASH, Duration::MAX)
                .await
                .unwrap_err()
                .to_string()
                .contains("too long")
        );
    }

    fn receipt_with(logs: Vec<Log>, success: bool) -> TransactionReceipt {
        use alloy::consensus::{Eip658Value, Receipt, ReceiptEnvelope, ReceiptWithBloom};
        TransactionReceipt {
            inner: ReceiptEnvelope::Eip1559(ReceiptWithBloom {
                receipt: Receipt {
                    status: Eip658Value::Eip658(success),
                    cumulative_gas_used: 200_000,
                    logs,
                },
                logs_bloom: Default::default(),
            }),
            transaction_hash: TX_HASH,
            transaction_index: Some(3),
            block_hash: Some(B256::repeat_byte(0x33)),
            block_number: Some(BLOCK_NUMBER),
            gas_used: 150_000,
            effective_gas_price: 1_000_000_000,
            blob_gas_used: None,
            blob_gas_price: None,
            from: DEPOSITOR,
            to: Some(addr(MAINNET.xreserve)),
            contract_address: None,
        }
    }

    #[tokio::test]
    async fn receipts_carry_the_decoded_deposit() {
        let (chain, asserter) = mocked();
        let approval_like = rpc_log(
            addr(MAINNET.usdc),
            alloy::primitives::LogData::new_unchecked(vec![B256::repeat_byte(0x55)], Bytes::new()),
            LOG_INDEX - 1,
        );
        let deposit = rpc_log(
            addr(MAINNET.xreserve),
            deposit_event().encode_log_data(),
            LOG_INDEX,
        );
        asserter.push_success(&receipt_with(
            vec![approval_like.clone(), deposit.clone()],
            true,
        ));
        let info = chain.receipt(TX_HASH).await.unwrap().unwrap();
        assert!(info.success);
        assert_eq!(info.block_number, Some(BLOCK_NUMBER));
        assert_eq!(info.deposit().unwrap(), &observed());

        asserter.push_success(&receipt_with(vec![approval_like], true));
        let not_a_deposit = chain.receipt(TX_HASH).await.unwrap().unwrap();
        assert!(not_a_deposit.deposit.is_none());
        assert!(not_a_deposit.deposit().is_err());

        asserter.push_success(&receipt_with(vec![deposit], false));
        let reverted = chain.wait_receipt(TX_HASH, RECEIPT_TIMEOUT).await.unwrap();
        assert!(!reverted.success);
        assert!(format!("{:#}", reverted.deposit().unwrap_err()).contains("reverted"));
    }

    #[tokio::test]
    async fn deposits_are_found_by_log_scan() {
        let (chain, asserter) = mocked();
        asserter.push_success(&U64::from(BLOCK_NUMBER));
        let mut removed = rpc_log(addr(MAINNET.xreserve), deposit_event().encode_log_data(), 1);
        removed.removed = true;
        asserter.push_success(&vec![
            removed,
            rpc_log(
                addr(MAINNET.xreserve),
                deposit_event().encode_log_data(),
                LOG_INDEX,
            ),
        ]);
        let found = chain
            .find_deposits(DEPOSITOR, PARTY, BLOCK_NUMBER)
            .await
            .unwrap();
        assert_eq!(found, vec![observed()]);
        assert!(asserter.read_q().is_empty());
    }

    fn release_log(source_depositor: Address, spec: u8, log_index: u64) -> Log {
        let event = IGatewayMinter::AttestationUsed {
            token: addr(MAINNET.usdc),
            recipient: DEPOSITOR,
            transferSpecHash: B256::repeat_byte(spec),
            sourceDomain: CANTON_DOMAIN,
            sourceDepositor: source_depositor.into_word(),
            sourceSigner: B256::repeat_byte(0x66),
            value: U256::from(5_000_000u64),
        };
        rpc_log(
            addr(MAINNET.gateway_minter),
            event.encode_log_data(),
            log_index,
        )
    }

    #[tokio::test]
    async fn releases_from_canton_only_with_range_halving() {
        let (chain, asserter) = mocked();
        asserter.push_success(&U64::from(9_999)); // eth_blockNumber
        asserter.push_failure_msg("query exceeds max block range"); // 0..=4999
        asserter.push_success(&vec![
            release_log(addr(MAINNET.canton_rdd), 0x01, 10),
            release_log(Address::repeat_byte(0x77), 0x02, 11),
        ]); // 0..=2499
        asserter.push_success(&Vec::<Log>::new()); // 2500..=4999
        asserter.push_success(&Vec::<Log>::new()); // 5000..=7499
        asserter.push_success(&vec![release_log(addr(MAINNET.canton_rdd), 0x03, 12)]); // 7500..=9999
        let releases = chain.find_releases(DEPOSITOR, 0).await.unwrap();
        assert!(asserter.read_q().is_empty());
        assert_eq!(releases.len(), 2);
        let first = &releases[0];
        assert_eq!(first.transfer_spec_hash, B256::repeat_byte(0x01));
        assert_eq!(first.log_index, 10);
        assert_eq!(first.block_number, BLOCK_NUMBER);
        assert_eq!(first.tx_hash, TX_HASH);
        assert_eq!(first.source_domain, CANTON_DOMAIN);
        assert_eq!(first.value, U256::from(5_000_000u64));
        assert_eq!(releases[1].transfer_spec_hash, B256::repeat_byte(0x03));
    }

    #[tokio::test]
    async fn log_scan_gives_up_at_the_minimum_range() {
        let (chain, asserter) = mocked();
        asserter.push_success(&U64::from(50));
        // 5000 -> 2500 -> 1250 -> 625 -> 312 -> 156 -> 100: seven refusals in all.
        for _ in 0..7 {
            asserter.push_failure_msg("range too large");
        }
        let err = chain.find_releases(DEPOSITOR, 0).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("eth_getLogs AttestationUsed 0..50"),
            "{err:#}"
        );
        assert!(asserter.read_q().is_empty());
    }
}
