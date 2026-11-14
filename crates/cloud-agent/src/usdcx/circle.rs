//! Circle xReserve API (no auth): deposit attestations and the withdrawal fee quote.
//! Used for status and fees only — never as a source of addresses.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};

use super::chain::{CANTON_DOMAIN, ETHEREUM_DOMAIN, EthNetwork};
use super::ethaddr::{hex0x, party_hash};

/// Longest wait for one xReserve API request.
pub const API_TIMEOUT: Duration = Duration::from_secs(60);

/// An HTTP client for the xReserve API, bounded by [`API_TIMEOUT`].
pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(API_TIMEOUT)
        .build()
        .context("xReserve API client")
}

#[derive(Debug, Clone)]
pub struct Attestation {
    pub payload: String,
    pub message_hash: String,
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow!("GET {url}: {e}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .context("read xReserve API response")?;
    ensure!(
        status.is_success(),
        "xReserve API {url} failed (HTTP {status}): {text}"
    );
    serde_json::from_str(&text).context("xReserve API response is not JSON")
}

/// Circle's attestations for a deposit tx; empty while Ethereum finality is pending.
pub async fn attestations_by_tx(
    client: &reqwest::Client,
    net: &EthNetwork,
    tx_hash: &str,
) -> Result<Vec<Attestation>> {
    let url = format!("{}/v1/attestations?txHash={tx_hash}", net.xreserve_api);
    let body = get_json(client, &url).await?;
    Ok(parse_attestations(&body))
}

/// The Canton-bound attestations of an `/v1/attestations` response.
fn parse_attestations(body: &Value) -> Vec<Attestation> {
    body.get("attestations")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|a| {
                    a.get("remoteDomain").and_then(Value::as_u64) == Some(u64::from(CANTON_DOMAIN))
                })
                .filter_map(|a| {
                    Some(Attestation {
                        payload: a.get("payload")?.as_str()?.to_string(),
                        message_hash: a.get("messageHash")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Circle's view of a withdrawal by Circle withdrawal id (404 -> None).
#[derive(Debug, Clone)]
pub struct Withdrawal {
    pub status: String,
    pub transfer_spec_hashes: Vec<String>,
}

pub async fn withdrawal(
    client: &reqwest::Client,
    net: &EthNetwork,
    id: &str,
) -> Result<Option<Withdrawal>> {
    let url = format!("{}/v1/withdrawal/{id}", net.xreserve_api);
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("GET {url}: {e}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let status = response.status();
    let text = response
        .text()
        .await
        .context("read xReserve API response")?;
    ensure!(
        status.is_success(),
        "xReserve API {url} failed (HTTP {status}): {text}"
    );
    let body: Value = serde_json::from_str(&text).context("xReserve API response is not JSON")?;
    Ok(Some(parse_withdrawal(&body)))
}

fn parse_withdrawal(body: &Value) -> Withdrawal {
    Withdrawal {
        status: body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        transfer_spec_hashes: body
            .get("transferSpecHashes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub async fn info(client: &reqwest::Client, net: &EthNetwork) -> Result<Value> {
    get_json(client, &format!("{}/v1/info", net.xreserve_api)).await
}

/// Circle's withdrawal fee for a Canton -> Ethereum burn (the same request
/// Digital Asset's bridge UI sends). `fee_included`: `amount` is the gross
/// burn (fee taken out of it) instead of the net.
pub async fn quote_withdrawal_fee(
    client: &reqwest::Client,
    net: &EthNetwork,
    party: &str,
    recipient_lower: &str,
    amount: Decimal,
    fee_included: bool,
) -> Result<Decimal> {
    let url = format!("{}/v1/prepare-withdrawal", net.xreserve_api);
    let request = prepare_withdrawal_request(party, recipient_lower, amount, fee_included);
    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|e| anyhow!("POST {url}: {e}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .context("read prepare-withdrawal response")?;
    ensure!(
        status.is_success(),
        "prepare-withdrawal failed (HTTP {status}): {text}"
    );
    let body: Value =
        serde_json::from_str(&text).context("prepare-withdrawal response is not JSON")?;
    parse_fee(&body)
}

/// The `prepare-withdrawal` request body: one batch for one burn.
fn prepare_withdrawal_request(
    party: &str,
    recipient_lower: &str,
    amount: Decimal,
    fee_included: bool,
) -> Value {
    let value_key = if fee_included {
        "valueIncludingFees"
    } else {
        "valueExcludingFees"
    };
    let mut batch = Map::new();
    batch.insert("token".into(), json!("USDC"));
    batch.insert("remoteDomain".into(), json!(CANTON_DOMAIN));
    batch.insert(
        "remoteDepositor".into(),
        json!(hex0x(party_hash(party).as_slice())),
    );
    batch.insert("finalDestinationDomain".into(), json!(ETHEREUM_DOMAIN));
    batch.insert(
        "finalDestinationRecipient".into(),
        json!(format!(
            "0x{:0>64}",
            recipient_lower.trim_start_matches("0x")
        )),
    );
    batch.insert("useCircleForwarding".into(), json!(true));
    batch.insert(value_key.into(), json!(amount.normalize().to_string()));
    json!({ "batches": [Value::Object(batch)] })
}

/// Sum of `maxFee` (USDC base units) over the burn intents of the first batch.
fn parse_fee(body: &Value) -> Result<Decimal> {
    let intents = body
        .pointer("/batches/0/burnIntents")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| anyhow!("prepare-withdrawal response has no burnIntents"))?;
    let mut units: u128 = 0;
    for intent in intents {
        let fee = intent
            .get("maxFee")
            .and_then(|v| match v {
                Value::String(s) => s.parse::<u128>().ok(),
                Value::Number(n) => n.as_u64().map(u128::from),
                _ => None,
            })
            .ok_or_else(|| anyhow!("burn intent without a numeric maxFee"))?;
        units = units
            .checked_add(fee)
            .ok_or_else(|| anyhow!("fee out of range"))?;
    }
    let units = i128::try_from(units).map_err(|_| anyhow!("fee out of range"))?;
    Decimal::try_from_i128_with_scale(units, 6)
        .map(|d| d.normalize())
        .map_err(|_| anyhow!("fee out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn fee_is_summed_over_burn_intents() {
        let body = json!({"batches": [{"burnIntents": [{"maxFee": "2000000"}, {"maxFee": 3500}]}]});
        assert_eq!(parse_fee(&body).unwrap(), Decimal::new(20035, 4));
        assert!(parse_fee(&json!({"batches": [{"burnIntents": []}]})).is_err());
        assert!(parse_fee(&json!({})).is_err());
        assert!(parse_fee(&json!({"batches": [{"burnIntents": [{"maxFee": "x"}]}]})).is_err());
        assert!(parse_fee(&json!({"batches": [{"burnIntents": [{"maxFee": -1}]}]})).is_err());
        // Overflow and values beyond Decimal's range are errors, not panics.
        let max = u128::MAX.to_string();
        assert!(
            parse_fee(&json!({"batches": [{"burnIntents": [{"maxFee": max}, {"maxFee": "1"}]}]}))
                .is_err()
        );
        let beyond_decimal = (1u128 << 100).to_string();
        assert!(
            parse_fee(&json!({"batches": [{"burnIntents": [{"maxFee": beyond_decimal}]}]}))
                .is_err()
        );
    }

    #[test]
    fn prepare_withdrawal_request_shape() {
        let party =
            "vector-party::12200123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let recipient = "0x1111111111111111111111111111111111111111";
        let net = prepare_withdrawal_request(
            party,
            recipient,
            Decimal::from_str("10.50").unwrap(),
            false,
        );
        let batch = &net["batches"][0];
        assert_eq!(batch["token"], "USDC");
        assert_eq!(batch["remoteDomain"], 10001);
        assert_eq!(
            batch["remoteDepositor"],
            hex0x(party_hash(party).as_slice())
        );
        assert_eq!(batch["finalDestinationDomain"], 0);
        assert_eq!(
            batch["finalDestinationRecipient"],
            format!("0x{}{}", "0".repeat(24), "11".repeat(20))
        );
        assert_eq!(batch["useCircleForwarding"], true);
        assert_eq!(batch["valueExcludingFees"], "10.5");
        assert!(batch.get("valueIncludingFees").is_none());
        assert_eq!(batch.as_object().unwrap().len(), 7);

        let gross = prepare_withdrawal_request(party, recipient, Decimal::from(3), true);
        assert_eq!(gross["batches"][0]["valueIncludingFees"], "3");
        assert!(gross["batches"][0].get("valueExcludingFees").is_none());
    }

    #[test]
    fn attestations_keep_canton_entries_only() {
        let body = json!({"attestations": [
            {"remoteDomain": 10001, "payload": "0x01", "messageHash": "0xaa"},
            {"remoteDomain": 7, "payload": "0x02", "messageHash": "0xbb"},
            {"remoteDomain": 10001, "payload": "0x03"},
        ]});
        let list = parse_attestations(&body);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].payload, "0x01");
        assert_eq!(list[0].message_hash, "0xaa");
        assert!(parse_attestations(&json!({})).is_empty());
    }

    #[test]
    fn withdrawal_fields() {
        let w = parse_withdrawal(
            &json!({"status": "COMPLETED", "transferSpecHashes": ["0xcc", 5, "0xdd"]}),
        );
        assert_eq!(w.status, "COMPLETED");
        assert_eq!(w.transfer_spec_hashes, vec!["0xcc", "0xdd"]);
        let empty = parse_withdrawal(&json!({}));
        assert_eq!(empty.status, "");
        assert!(empty.transfer_spec_hashes.is_empty());
    }
}
