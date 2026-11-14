//! Ethereum and Circle values of the xReserve bridge, pinned per network.
//!
//! Contract addresses are pinned here on purpose: API responses are never
//! used as a source of contract addresses. Sources: Circle's xReserve
//! documentation, Digital Asset's USDCx documentation and on-chain reads.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use alloy_primitives::{Address, B256};
use anyhow::{Result, anyhow, bail};

/// Circle domain id of Canton (the deposit's `remoteDomain`).
pub const CANTON_DOMAIN: u32 = 10001;
/// Circle domain id of Ethereum (a withdrawal's `destinationDomain`).
/// Domain 1 is Avalanche: never use it for Ethereum.
pub const ETHEREUM_DOMAIN: u32 = 0;
/// Digital Asset's open-source deposit page (WalletConnect), an alternative
/// to sending raw calldata.
pub const DEPOSIT_UI_URL: &str = "https://digital-asset.github.io/xreserve-deposits/";

/// The Ethereum chain, contracts and Circle API of one network.
///
/// Addresses are EIP-55 strings and the token id a 0x-prefixed bytes32;
/// [`EthNetwork::check_pins`] validates them and
/// [`super::ethaddr::addr`] / [`super::ethaddr::b256`] convert them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthNetwork {
    /// `mainnet` or `testnet`.
    pub name: &'static str,
    /// Circle xReserve API base URL (no trailing slash).
    pub xreserve_api: &'static str,
    /// The chain id the Ethereum RPC endpoint must report.
    pub eth_chain_id: u64,
    pub eth_chain_name: &'static str,
    /// Block explorer prefix of a transaction link (the hash is appended).
    pub explorer_tx: &'static str,
    /// xReserve proxy. The same address also hosts xReserve on other chains,
    /// so the chain id is always checked as well.
    pub xreserve: &'static str,
    pub usdc: &'static str,
    /// Emits `AttestationUsed` when Circle releases a withdrawal.
    pub gateway_minter: &'static str,
    /// Canton RemoteDomainDepositor: the `sourceDepositor` of the releases
    /// of Canton withdrawals.
    pub canton_rdd: &'static str,
    /// bytes32 token id of USDCx (= `getRemoteToken(10001, USDC)` = the
    /// `tokenId` of the Canton bridge agreement).
    pub canton_remote_token: &'static str,
}

pub static MAINNET: EthNetwork = EthNetwork {
    name: "mainnet",
    xreserve_api: "https://xreserve-api.circle.com",
    eth_chain_id: 1,
    eth_chain_name: "Ethereum mainnet",
    explorer_tx: "https://etherscan.io/tx/",
    xreserve: "0x8888888199b2Df864bf678259607d6D5EBb4e3Ce",
    usdc: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
    gateway_minter: "0x2222222d7164433c4C09B0b0D809a9b52C04C205",
    canton_rdd: "0x427761a14f15629e3B550FBe6740cf16d2d29669",
    canton_remote_token: "0x661237037dc811823d8b2de17aaabb8ef2ac9b713ca7db3b01fc7f7baf7db562",
};

pub static TESTNET: EthNetwork = EthNetwork {
    name: "testnet",
    xreserve_api: "https://xreserve-api-testnet.circle.com",
    eth_chain_id: 11155111,
    eth_chain_name: "Ethereum Sepolia",
    explorer_tx: "https://sepolia.etherscan.io/tx/",
    xreserve: "0x008888878f94C0d87defdf0B07f46B93C1934442",
    usdc: "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
    gateway_minter: "0x0022222ABE238Cc2C7Bb1f21003F0a260052475B",
    canton_rdd: "0xcc2cBF0357b5A64b0c7F0DcBB49D309F3aBB96D2",
    canton_remote_token: "0x74ed63088c070c8fd5d8ad71f2a1cef868c63d00e0ac6dc2a6722d171691a422",
};

impl EthNetwork {
    /// `mainnet` or `testnet`, ignoring case and surrounding spaces.
    pub fn by_name(name: &str) -> Result<&'static EthNetwork> {
        match name.trim().to_ascii_lowercase().as_str() {
            "mainnet" => Ok(&MAINNET),
            "testnet" => Ok(&TESTNET),
            other => bail!(
                "xReserve is not available on network {other:?}: use mainnet or testnet \
                 (DevNet has no xReserve)"
            ),
        }
    }

    /// Fails unless every pinned address is a nonzero EIP-55 (checksummed)
    /// address and the token id a nonzero bytes32.
    pub fn check_pins(&self) -> Result<()> {
        for (what, pin) in [
            ("xReserve", self.xreserve),
            ("USDC", self.usdc),
            ("GatewayMinter", self.gateway_minter),
            ("Canton RemoteDomainDepositor", self.canton_rdd),
        ] {
            let address = Address::parse_checksummed(pin, None).map_err(|_| {
                anyhow!(
                    "pinned {what} address {pin} ({}) is not a checksummed address",
                    self.name
                )
            })?;
            if address == Address::ZERO {
                bail!("pinned {what} address ({}) is the zero address", self.name);
            }
        }
        let token = self
            .canton_remote_token
            .parse::<B256>()
            .map_err(|_| anyhow!("pinned USDCx token id ({}) is not a bytes32", self.name))?;
        if token == B256::ZERO {
            bail!("pinned USDCx token id ({}) is zero", self.name);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_selection() {
        assert_eq!(EthNetwork::by_name("mainnet").unwrap().eth_chain_id, 1);
        assert_eq!(EthNetwork::by_name(" MainNet ").unwrap().name, "mainnet");
        assert_eq!(
            EthNetwork::by_name("TESTNET").unwrap().eth_chain_id,
            11155111
        );
        assert!(EthNetwork::by_name("devnet").is_err());
        assert!(EthNetwork::by_name("").is_err());
    }

    #[test]
    fn pins_are_valid() {
        for net in [&MAINNET, &TESTNET] {
            net.check_pins().unwrap();
            for pin in [net.xreserve, net.usdc, net.gateway_minter, net.canton_rdd] {
                // Every pin is already in its EIP-55 form.
                let address = Address::parse_checksummed(pin, None).unwrap();
                assert_eq!(address.to_checksum(None), pin);
            }
            assert_eq!(
                net.canton_remote_token,
                net.canton_remote_token.to_ascii_lowercase()
            );
            assert!(!net.xreserve_api.ends_with('/'));
            assert!(net.explorer_tx.ends_with("/tx/"));
        }
        assert_ne!(MAINNET.canton_remote_token, TESTNET.canton_remote_token);
        assert_ne!(MAINNET.eth_chain_id, TESTNET.eth_chain_id);
    }

    #[test]
    fn broken_pins_are_reported() {
        let wrong_case = EthNetwork {
            usdc: "0xa0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            ..MAINNET.clone()
        };
        assert!(wrong_case.check_pins().is_err());
        let zero = EthNetwork {
            canton_rdd: "0x0000000000000000000000000000000000000000",
            ..MAINNET.clone()
        };
        assert!(zero.check_pins().is_err());
        let short_token = EthNetwork {
            canton_remote_token: "0x6612",
            ..MAINNET.clone()
        };
        assert!(short_token.check_pins().is_err());
    }
}
