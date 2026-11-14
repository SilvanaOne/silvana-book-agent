//! Ethereum addresses, USDC amounts and the xReserve deposit intent: the
//! pure parts of the Ethereum side (no RPC, no ABI).

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use anyhow::{Result, anyhow, bail, ensure};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use super::chain::{CANTON_DOMAIN, ETHEREUM_DOMAIN, EthNetwork};

/// Deposit-intent magic and version (Circle xReserve technical guide; the
/// Canton bridge decodes the same layout).
const DEPOSIT_INTENT_MAGIC: [u8; 4] = [0x5a, 0x2e, 0x0a, 0xcd];
const DEPOSIT_INTENT_VERSION: u32 = 1;
/// Circle domain of the deposit's source chain (Ethereum), used in the nonce.
const ETHEREUM_SOURCE_DOMAIN: u32 = ETHEREUM_DOMAIN;
/// Fixed part of the payload, up to and including `hookDataLen`.
const PAYLOAD_FIXED_LEN: usize = 240;
/// USDC has 6 decimals.
const USDC_DECIMALS: u32 = 6;

/// A pinned address constant (an [`EthNetwork`] field). The pins are checked
/// by [`EthNetwork::check_pins`] (and when connecting); a string that does not
/// parse yields the zero address, for which nothing is ever signed.
/// User input goes through [`parse_eth_address`] instead.
pub fn addr(s: &str) -> Address {
    s.parse().unwrap_or(Address::ZERO)
}

/// A pinned bytes32 constant; zero when the string does not parse (see [`addr`]).
pub fn b256(s: &str) -> B256 {
    s.parse().unwrap_or(B256::ZERO)
}

/// `remoteRecipient` of a Canton deposit = keccak256 of the party id
/// (the bridge agreement's `userHash`).
pub fn party_hash(party: &str) -> B256 {
    keccak256(party.as_bytes())
}

pub fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// Lowercase 0x-prefixed form (what a burn sends as `destinationRecipient`).
pub fn lower(address: Address) -> String {
    hex0x(address.as_slice())
}

/// Compare hex strings ignoring case and an optional `0x` prefix.
pub fn same_hex(a: &str, b: &str) -> bool {
    let strip = |s: &str| s.trim().trim_start_matches("0x").to_ascii_lowercase();
    strip(a) == strip(b)
}

/// 20-byte address, EIP-55 checked when mixed-case, never the zero address.
pub fn parse_eth_address(input: &str) -> Result<Address> {
    let s = input.trim();
    let Some(hex_part) = s.strip_prefix("0x") else {
        bail!("Ethereum address must start with 0x: {s}");
    };
    ensure!(
        hex_part.len() == 40 && hex_part.chars().all(|c| c.is_ascii_hexdigit()),
        "Ethereum address must be 0x followed by 40 hex characters: {s}"
    );
    let mixed_case = hex_part.chars().any(|c| c.is_ascii_lowercase())
        && hex_part.chars().any(|c| c.is_ascii_uppercase());
    let address = if mixed_case {
        Address::parse_checksummed(s, None)
            .map_err(|_| anyhow!("Ethereum address {s} has an invalid EIP-55 checksum"))?
    } else {
        s.parse::<Address>()
            .map_err(|e| anyhow!("invalid Ethereum address {s}: {e}"))?
    };
    ensure!(address != Address::ZERO, "the zero address is not allowed");
    Ok(address)
}

/// The Ethereum address in a left-padded 32-byte field (lowercase,
/// 0x-prefixed), e.g. a deposit attestation's `sourceDepositor`. A bare
/// 20-byte address is accepted as well.
pub fn address_from_word(word: &str) -> Option<String> {
    let h = word.trim().trim_start_matches("0x").to_ascii_lowercase();
    let tail = match h.len() {
        40 => Some(h.as_str()),
        64 => match (h.get(..24), h.get(24..)) {
            (Some(padding), Some(tail)) if padding.chars().all(|c| c == '0') => Some(tail),
            _ => None,
        },
        _ => None,
    }?;
    tail.chars()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| format!("0x{tail}"))
}

/// USDC base units (6 decimals); rejects non-positive amounts and more than 6 decimals.
pub fn usdc_units(amount: Decimal) -> Result<U256> {
    ensure!(amount > Decimal::ZERO, "amount must be positive");
    let scaled = amount
        .checked_mul(Decimal::from(1_000_000u32))
        .ok_or_else(|| anyhow!("amount {amount} is out of range"))?;
    ensure!(
        scaled.fract().is_zero(),
        "amount {amount} has more than 6 decimal places"
    );
    let units = scaled
        .trunc()
        .to_u128()
        .ok_or_else(|| anyhow!("amount {amount} is out of range"))?;
    Ok(U256::from(units))
}

/// USDC base units as a decimal amount (6 decimals, normalized).
pub fn units_to_decimal(units: U256) -> Result<Decimal> {
    let out_of_range = || anyhow!("token amount {units} is out of range");
    let v = u128::try_from(units).map_err(|_| out_of_range())?;
    let v = i128::try_from(v).map_err(|_| out_of_range())?;
    Decimal::try_from_i128_with_scale(v, USDC_DECIMALS)
        .map(|d| d.normalize())
        .map_err(|_| out_of_range())
}

/// Wei as ETH (saturates at `Decimal::MAX`).
pub fn wei_to_eth(wei: U256) -> Decimal {
    u128::try_from(wei)
        .ok()
        .and_then(|v| i128::try_from(v).ok())
        .and_then(|v| Decimal::try_from_i128_with_scale(v, 18).ok())
        .map(|d| d.normalize())
        .unwrap_or(Decimal::MAX)
}

/// Deposit-intent nonce: keccak256(abi.encode(uint32 0) ‖ txHash ‖ abi.encode(uint256 logIndex)),
/// with the block-level log index of the `DepositedToRemote` log.
pub fn deposit_nonce(tx_hash: B256, log_index: u64) -> B256 {
    let mut buf = Vec::with_capacity(96);
    buf.extend_from_slice(&U256::from(ETHEREUM_SOURCE_DOMAIN).to_be_bytes::<32>());
    buf.extend_from_slice(tx_hash.as_slice());
    buf.extend_from_slice(&U256::from(log_index).to_be_bytes::<32>());
    keccak256(&buf)
}

/// The deposit intent Circle attests and the Canton bridge decodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepositIntent {
    pub amount: U256,
    pub remote_domain: u32,
    pub remote_token: B256,
    pub remote_recipient: B256,
    pub local_token: Address,
    pub local_depositor: Address,
    pub max_fee: U256,
    pub nonce: B256,
    pub hook_data: Bytes,
}

impl DepositIntent {
    /// encodePacked(magic, version, amount, remoteDomain, remoteToken, remoteRecipient,
    /// localToken, localDepositor, maxFee, nonce, hookDataLen, hookData).
    pub fn payload(&self) -> Vec<u8> {
        // hookData is a party id (well under 4 GiB); an oversized one cannot
        // be encoded and saturates the length field.
        let hook_len = u32::try_from(self.hook_data.len()).unwrap_or(u32::MAX);
        let mut p = Vec::with_capacity(PAYLOAD_FIXED_LEN.saturating_add(self.hook_data.len()));
        p.extend_from_slice(&DEPOSIT_INTENT_MAGIC);
        p.extend_from_slice(&DEPOSIT_INTENT_VERSION.to_be_bytes());
        p.extend_from_slice(&self.amount.to_be_bytes::<32>());
        p.extend_from_slice(&self.remote_domain.to_be_bytes());
        p.extend_from_slice(self.remote_token.as_slice());
        p.extend_from_slice(self.remote_recipient.as_slice());
        p.extend_from_slice(self.local_token.into_word().as_slice());
        p.extend_from_slice(self.local_depositor.into_word().as_slice());
        p.extend_from_slice(&self.max_fee.to_be_bytes::<32>());
        p.extend_from_slice(self.nonce.as_slice());
        p.extend_from_slice(&hook_len.to_be_bytes());
        p.extend_from_slice(&self.hook_data);
        p
    }

    /// The key of Circle's attestation (`GET /v1/attestations/{messageHash}`).
    pub fn message_hash(&self) -> B256 {
        keccak256(self.payload())
    }

    /// Decode a payload from Circle's attestation API.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        ensure!(
            payload.len() >= PAYLOAD_FIXED_LEN,
            "deposit intent payload too short"
        );
        ensure!(
            payload.get(..4) == Some(DEPOSIT_INTENT_MAGIC.as_slice()),
            "unknown deposit intent magic"
        );
        ensure!(
            u32_at(payload, 4)? == DEPOSIT_INTENT_VERSION,
            "unknown deposit intent version"
        );
        let hook_len = usize::try_from(u32_at(payload, 236)?)
            .map_err(|_| anyhow!("deposit intent hook length mismatch"))?;
        ensure!(
            PAYLOAD_FIXED_LEN.checked_add(hook_len) == Some(payload.len()),
            "deposit intent hook length mismatch"
        );
        let hook_data = payload
            .get(PAYLOAD_FIXED_LEN..)
            .ok_or_else(|| anyhow!("deposit intent payload too short"))?;
        Ok(Self {
            amount: U256::from_be_bytes(word_at(payload, 8)?.0),
            remote_domain: u32_at(payload, 40)?,
            remote_token: word_at(payload, 44)?,
            remote_recipient: word_at(payload, 76)?,
            local_token: Address::from_word(word_at(payload, 108)?),
            local_depositor: Address::from_word(word_at(payload, 140)?),
            max_fee: U256::from_be_bytes(word_at(payload, 172)?.0),
            nonce: word_at(payload, 204)?,
            hook_data: Bytes::copy_from_slice(hook_data),
        })
    }

    /// The depositor, unless the field is the zero address.
    pub fn local_depositor_address(&self) -> Option<Address> {
        (self.local_depositor != Address::ZERO).then_some(self.local_depositor)
    }

    /// The Canton party carried in `hookData`.
    pub fn party(&self) -> Option<String> {
        String::from_utf8(self.hook_data.to_vec()).ok()
    }

    /// Everything the Canton bridge validates, plus the hookData it reads.
    pub fn verify_for(&self, net: &EthNetwork, party: &str) -> Result<()> {
        ensure!(
            self.remote_domain == CANTON_DOMAIN,
            "deposit remoteDomain is {}, not Canton ({CANTON_DOMAIN})",
            self.remote_domain
        );
        ensure!(
            self.remote_recipient == party_hash(party),
            "deposit remoteRecipient {} is not keccak256({party})",
            self.remote_recipient
        );
        ensure!(
            self.hook_data.as_ref() == party.as_bytes(),
            "deposit hookData is not the party id {party}"
        );
        ensure!(
            self.local_token == addr(net.usdc),
            "deposit localToken {} is not USDC",
            self.local_token
        );
        ensure!(
            self.remote_token == b256(net.canton_remote_token),
            "deposit remoteToken {} is not USDCx on Canton",
            self.remote_token
        );
        ensure!(
            self.amount >= self.max_fee,
            "deposit maxFee exceeds the amount"
        );
        Ok(())
    }
}

/// `N` bytes of `payload` at `offset`.
fn bytes_at<const N: usize>(payload: &[u8], offset: usize) -> Result<[u8; N]> {
    offset
        .checked_add(N)
        .and_then(|end| payload.get(offset..end))
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or_else(|| anyhow!("deposit intent payload too short"))
}

fn u32_at(payload: &[u8], offset: usize) -> Result<u32> {
    bytes_at::<4>(payload, offset).map(u32::from_be_bytes)
}

fn word_at(payload: &[u8], offset: usize) -> Result<B256> {
    bytes_at::<32>(payload, offset).map(B256::from)
}

#[cfg(test)]
mod tests {
    use super::super::chain::MAINNET;
    use super::*;
    use std::str::FromStr;

    /// A neutral, made-up party id.
    const PARTY: &str =
        "vector-party::12200123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn intent() -> DepositIntent {
        DepositIntent {
            amount: U256::from(12_345_678u64),
            remote_domain: CANTON_DOMAIN,
            remote_token: b256(MAINNET.canton_remote_token),
            remote_recipient: party_hash(PARTY),
            local_token: addr(MAINNET.usdc),
            local_depositor: Address::repeat_byte(0x11),
            max_fee: U256::ZERO,
            nonce: deposit_nonce(B256::repeat_byte(0x22), 42),
            hook_data: Bytes::copy_from_slice(PARTY.as_bytes()),
        }
    }

    #[test]
    fn pinned_constants() {
        assert_eq!(
            lower(addr(MAINNET.usdc)),
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
        );
        assert_eq!(
            b256(MAINNET.canton_remote_token).to_string(),
            MAINNET.canton_remote_token
        );
        // Never a panic: what does not parse is zero, which is never signed for.
        assert_eq!(addr("not an address"), Address::ZERO);
        assert_eq!(b256("0x12"), B256::ZERO);
    }

    #[test]
    fn eth_address_validation() {
        let usdc = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
        assert_eq!(
            lower(parse_eth_address(usdc).unwrap()),
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
        );
        assert!(parse_eth_address(&usdc.to_ascii_lowercase()).is_ok());
        assert!(
            parse_eth_address(&format!(
                " {} ",
                usdc.to_ascii_uppercase().replace("0X", "0x")
            ))
            .is_ok()
        );
        assert!(parse_eth_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB49").is_err());
        assert!(parse_eth_address("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb4").is_err());
        assert!(parse_eth_address("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48").is_err());
        assert!(parse_eth_address("0x0000000000000000000000000000000000000000").is_err());
        assert!(parse_eth_address("0xé0b86991c6218b36c1d19d4a2e9eb0ce3606eb4").is_err());
    }

    #[test]
    fn usdc_amounts() {
        assert_eq!(
            usdc_units(Decimal::from_str("1.5").unwrap()).unwrap(),
            U256::from(1_500_000u64)
        );
        assert_eq!(
            usdc_units(Decimal::from_str("0.000001").unwrap()).unwrap(),
            U256::from(1u64)
        );
        assert_eq!(
            usdc_units(Decimal::from_str("2.500000000").unwrap()).unwrap(),
            U256::from(2_500_000u64)
        );
        assert!(usdc_units(Decimal::from_str("0.0000001").unwrap()).is_err());
        assert!(usdc_units(Decimal::from_str("1.0000000000000000000000000001").unwrap()).is_err());
        assert!(usdc_units(Decimal::ZERO).is_err());
        assert!(usdc_units(Decimal::from_str("-1").unwrap()).is_err());
        assert!(usdc_units(Decimal::MAX).is_err());
        assert_eq!(
            units_to_decimal(U256::from(2_000_000u64)).unwrap(),
            Decimal::from(2)
        );
        assert_eq!(
            units_to_decimal(U256::from(12_345_678u64)).unwrap(),
            Decimal::from_str("12.345678").unwrap()
        );
        assert!(units_to_decimal(U256::from(u128::MAX)).is_err());
        assert!(units_to_decimal(U256::MAX).is_err());
    }

    #[test]
    fn wei_amounts() {
        assert_eq!(
            wei_to_eth(U256::from(1_500_000_000_000_000_000u128)),
            Decimal::from_str("1.5").unwrap()
        );
        assert_eq!(wei_to_eth(U256::MAX), Decimal::MAX);
    }

    #[test]
    fn hex_helpers() {
        assert_eq!(hex0x(&[0xab, 0x01]), "0xab01");
        assert!(same_hex("0xAB01", " ab01 "));
        assert!(!same_hex("0xab01", "0xab02"));
    }

    #[test]
    fn source_depositor_words() {
        let padded = "0000000000000000000000001111111111111111111111111111111111111111";
        assert_eq!(
            address_from_word(padded).as_deref(),
            Some("0x1111111111111111111111111111111111111111")
        );
        assert_eq!(
            address_from_word("0xAbCdEf0123456789aBcDeF0123456789AbCdEf01").as_deref(),
            Some("0xabcdef0123456789abcdef0123456789abcdef01")
        );
        assert!(
            address_from_word("1100000000000000000000001111111111111111111111111111111111111111")
                .is_none()
        );
        assert!(address_from_word("0x1234").is_none());
        // 64 bytes, but not a character boundary at the padding split.
        assert!(address_from_word(&format!("{}é{}", "0".repeat(23), "1".repeat(39))).is_none());
    }

    #[test]
    fn deposit_nonce_is_the_abi_encoded_hash() {
        // abi.encode(uint32 0) ‖ txHash ‖ abi.encode(uint256 logIndex), spelled out.
        let tx = B256::repeat_byte(0x22);
        let mut words = [0u8; 96];
        for (i, b) in words.iter_mut().enumerate() {
            if (32..64).contains(&i) {
                *b = 0x22;
            }
        }
        words[95] = 42;
        assert_eq!(deposit_nonce(tx, 42), keccak256(words));
        assert_ne!(deposit_nonce(tx, 42), deposit_nonce(tx, 43));
    }

    #[test]
    fn payload_round_trips_and_rejects_malformed_input() {
        let intent = intent();
        let payload = intent.payload();
        assert_eq!(payload.len(), PAYLOAD_FIXED_LEN + PARTY.len());
        assert_eq!(DepositIntent::decode(&payload).unwrap(), intent);
        assert_eq!(intent.party().as_deref(), Some(PARTY));
        assert_eq!(
            intent.local_depositor_address(),
            Some(Address::repeat_byte(0x11))
        );
        intent.verify_for(&MAINNET, PARTY).unwrap();
        let no_depositor = DepositIntent {
            local_depositor: Address::ZERO,
            ..intent.clone()
        };
        assert_eq!(no_depositor.local_depositor_address(), None);

        assert!(DepositIntent::decode(&payload[..PAYLOAD_FIXED_LEN - 1]).is_err());
        assert!(DepositIntent::decode(&payload[..payload.len() - 1]).is_err());
        let mut longer = payload.clone();
        longer.push(0);
        assert!(DepositIntent::decode(&longer).is_err());
        let mut bad_magic = payload.clone();
        bad_magic[0] ^= 1;
        assert!(DepositIntent::decode(&bad_magic).is_err());
        let mut bad_version = payload.clone();
        bad_version[7] = 2;
        assert!(DepositIntent::decode(&bad_version).is_err());
        let mut huge_hook = payload;
        for (b, v) in huge_hook[236..240].iter_mut().zip(u32::MAX.to_be_bytes()) {
            *b = v;
        }
        assert!(DepositIntent::decode(&huge_hook).is_err());
    }

    #[test]
    fn verify_for_rejects_mismatches() {
        let ok = intent();
        assert!(ok.verify_for(&MAINNET, "other-party::1220ab").is_err());
        let mut wrong_domain = ok.clone();
        wrong_domain.remote_domain = 1;
        assert!(wrong_domain.verify_for(&MAINNET, PARTY).is_err());
        let mut wrong_token = ok.clone();
        wrong_token.local_token = Address::repeat_byte(0x33);
        assert!(wrong_token.verify_for(&MAINNET, PARTY).is_err());
        assert!(ok.verify_for(&super::super::chain::TESTNET, PARTY).is_err());
        let mut fee_above_amount = ok;
        fee_above_amount.max_fee = fee_above_amount.amount + U256::from(1u64);
        assert!(fee_above_amount.verify_for(&MAINNET, PARTY).is_err());
    }
}
