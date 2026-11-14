//! The party's bridge contracts as the ledger service returns them, and scans
//! of its transactions.
//!
//! A bridge contract counts only when its template is a `utility-bridge-v0`
//! template of an allowed package and it names the DA parties pinned for the
//! network: anyone can create a look-alike contract that the party observes,
//! but not one of DA's package without DA's signatures.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::str::FromStr;

use anyhow::{Result, anyhow, bail, ensure};
use orderbook_proto::ledger::{
    ActiveContractInfo, LedgerTransaction, get_updates_response, ledger_event,
};
use rust_decimal::Decimal;
use serde_json::Value;
use tx_verifier::usdcx::{
    BRIDGE_PACKAGE_IDS, HOLDING_SCHEME, INSTRUMENT_ID, T_ACCEPTED_WITHDRAW, T_AGREEMENT,
    T_AGREEMENT_REQUEST, T_DEPOSIT_ATTESTATION, T_HOLDING, T_REVOKED_WITHDRAW,
    T_WITHDRAW_ATTESTATION, UsdcxNetwork,
};

use super::chain::{CANTON_DOMAIN, EthNetwork};
use super::ethaddr::{address_from_word, party_hash, same_hex};
use crate::ledger_client::DAppProviderClient;
use crate::prost_struct_to_json;

/// `utility-bridge-v0:Module:Entity` by package name, for the ledger's filters.
fn bridge_filter((module, entity): (&str, &str)) -> String {
    format!("#utility-bridge-v0:{module}:{entity}")
}

/// Rejected onboarding requests (only read, never exercised).
const T_REJECTED_REQUEST: (&str, &str) = (
    "Utility.Bridge.V0.Agreement.User",
    "RejectedBridgeUserAgreementRequest",
);

/// `(package id, module, entity)` of a result's template id.
fn split_template(template_id: &str) -> Option<(&str, &str, &str)> {
    let mut parts = template_id.splitn(3, ':');
    Some((parts.next()?, parts.next()?, parts.next()?))
}

/// A bridge template of an allowed package.
fn is_bridge(template_id: &str, (module, entity): (&str, &str)) -> bool {
    split_template(template_id)
        .is_some_and(|(pkg, m, e)| m == module && e == entity && BRIDGE_PACKAGE_IDS.contains(&pkg))
}

fn is_template(template_id: &str, (module, entity): (&str, &str)) -> bool {
    split_template(template_id).is_some_and(|(_, m, e)| m == module && e == entity)
}

// ---------------------------------------------------------------------------
// Field readers. Arguments arrive as JSON: numerics as strings, Int64 as
// numbers (which may read back as floats), `None` as null or left out.
// ---------------------------------------------------------------------------

pub(crate) fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing text field {key}"))
}

pub(crate) fn decimal(v: &Value, key: &str) -> Result<Decimal> {
    let s = match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => bail!("missing numeric field {key}"),
    };
    Decimal::from_str(&s).map_err(|e| anyhow!("field {key}={s} is not a decimal: {e}"))
}

pub(crate) fn int(v: &Value, key: &str) -> Result<i64> {
    match v.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15)
                    .map(|f| f as i64)
            })
            .ok_or_else(|| anyhow!("field {key} is not an Int64")),
        Some(Value::String(s)) => s
            .parse()
            .map_err(|_| anyhow!("field {key}={s} is not an Int64")),
        _ => bail!("missing Int64 field {key}"),
    }
}

fn boolean(v: &Value, key: &str) -> Result<bool> {
    v.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("missing Bool field {key}"))
}

/// `ValidParty`: `Valid party` → the party; `Invalid _` → None.
fn valid_party(v: &Value, key: &str) -> Option<String> {
    let variant = v.get(key)?;
    let tag = variant
        .get("tag")
        .or_else(|| variant.get("constructor"))
        .and_then(Value::as_str)?;
    (tag == "Valid")
        .then(|| {
            variant
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten()
}

/// DA's parties and the USDCx instrument, as pinned for the network.
fn parties_match(v: &Value, net: &UsdcxNetwork) -> bool {
    let instrument = v.get("instrumentId");
    text(v, "crossChainRepresentative").ok() == Some(net.admin)
        && text(v, "bridgeOperator").ok() == Some(net.bridge_operator)
        && text(v, "operator").ok() == Some(net.utility_operator)
        && instrument.and_then(|i| text(i, "admin").ok()) == Some(net.admin)
        && instrument.and_then(|i| text(i, "id").ok()) == Some(INSTRUMENT_ID)
}

// ---------------------------------------------------------------------------
// Contracts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Agreement {
    pub cid: String,
    pub withdrawal_attester: String,
    pub user_hash: String,
    pub token_id: String,
    pub domain_id: i64,
    pub min_burn_value: Decimal,
    pub pre_approval: bool,
}

impl Agreement {
    fn from_args(cid: &str, v: &Value) -> Result<Self> {
        Ok(Self {
            cid: cid.to_string(),
            withdrawal_attester: text(v, "withdrawalAttester")?.to_string(),
            user_hash: text(v, "userHash")?.to_string(),
            token_id: text(v, "tokenId")?.to_string(),
            domain_id: int(v, "domainId")?,
            min_burn_value: decimal(v, "minBurnValue")?,
            pre_approval: boolean(v, "preApproval")?,
        })
    }

    /// The agreement must match what deposits to the party are built from.
    pub fn verify(&self, eth: &EthNetwork, party: &str) -> Result<()> {
        ensure!(
            same_hex(&self.user_hash, &party_hash(party).to_string()),
            "agreement {} userHash {} is not keccak256({party})",
            self.cid,
            self.user_hash
        );
        ensure!(
            same_hex(&self.token_id, eth.canton_remote_token),
            "agreement {} tokenId {} is not the USDCx remote token {}",
            self.cid,
            self.token_id,
            eth.canton_remote_token
        );
        ensure!(
            self.domain_id == i64::from(CANTON_DOMAIN),
            "agreement {} domainId {} is not Canton ({CANTON_DOMAIN})",
            self.cid,
            self.domain_id
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DepositAttestation {
    pub cid: String,
    pub amount: Decimal,
    /// The Ethereum depositor (lowercase 0x-address) when the word holds one.
    pub depositor: Option<String>,
    pub reference: String,
}

#[derive(Debug, Clone)]
pub struct Withdrawal {
    pub cid: String,
    pub request_id: String,
    pub amount: Decimal,
    pub destination: String,
    /// AcceptedWithdrawAttestation.traceId / RevokedWithdrawAttestation.reason.
    pub detail: Option<String>,
}

impl Withdrawal {
    fn from_attestation(cid: &str, att: &Value, detail: Option<String>) -> Result<Self> {
        ensure!(
            int(att, "destinationDomain")? == tx_verifier::usdcx::ETHEREUM_DOMAIN,
            "withdrawal {cid} is not to Ethereum"
        );
        Ok(Self {
            cid: cid.to_string(),
            request_id: text(att, "requestId")?.to_string(),
            amount: decimal(att, "amount")?,
            destination: text(att, "destinationRecipient")?.to_string(),
            detail,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Holding {
    pub cid: String,
    pub amount: Decimal,
    pub locked: bool,
}

/// The pinned USDCx of the party: registrar, instrument source, id and scheme.
fn usdcx_holding(v: &Value, party: &str, net: &UsdcxNetwork) -> bool {
    let instrument = v.get("instrument");
    text(v, "owner").ok() == Some(party)
        && text(v, "registrar").ok() == Some(net.admin)
        && instrument.and_then(|i| text(i, "source").ok()) == Some(net.admin)
        && instrument.and_then(|i| text(i, "id").ok()) == Some(INSTRUMENT_ID)
        && instrument.and_then(|i| text(i, "scheme").ok()) == Some(HOLDING_SCHEME)
}

/// Reads the party's bridge contracts.
pub struct Reader<'a> {
    pub client: &'a mut DAppProviderClient,
    pub net: &'static UsdcxNetwork,
    pub party: &'a str,
}

/// The arguments of the contracts of one template.
async fn contracts(
    client: &mut DAppProviderClient,
    filter: String,
    keep: impl Fn(&str) -> bool,
) -> Result<Vec<(String, Value)>> {
    let all: Vec<ActiveContractInfo> = client.get_active_contracts(&[filter]).await?;
    Ok(all
        .into_iter()
        .filter(|c| keep(&c.template_id))
        .filter_map(|c| {
            c.create_arguments
                .as_ref()
                .map(|args| (c.contract_id.clone(), prost_struct_to_json(args)))
        })
        .collect())
}

impl Reader<'_> {
    async fn bridge(&mut self, t: (&'static str, &'static str)) -> Result<Vec<(String, Value)>> {
        contracts(self.client, bridge_filter(t), |id| is_bridge(id, t)).await
    }

    /// The party's BridgeUserAgreements with DA's parties.
    pub async fn agreements(&mut self) -> Result<Vec<Agreement>> {
        let party = self.party;
        let net = self.net;
        self.bridge(T_AGREEMENT)
            .await?
            .into_iter()
            .filter(|(_, v)| text(v, "user").ok() == Some(party) && parties_match(v, net))
            .map(|(cid, v)| Agreement::from_args(&cid, &v))
            .collect()
    }

    /// The party's agreement: the first when several exist (nothing on the
    /// ledger keeps them unique).
    pub async fn agreement(&mut self) -> Result<Option<Agreement>> {
        let mut all = self.agreements().await?;
        if all.len() > 1 {
            tracing::warn!(
                count = all.len(),
                party = self.party,
                "several BridgeUserAgreements"
            );
        }
        Ok((!all.is_empty()).then(|| all.remove(0)))
    }

    /// The verified agreement, or why there is none.
    pub async fn require_agreement(&mut self, eth: &EthNetwork) -> Result<Agreement> {
        let party = self.party;
        let agreement = self.agreement().await?.ok_or_else(|| {
            anyhow!("{party} is not onboarded to USDCx — run `usdcx onboard` first")
        })?;
        agreement.verify(eth, party)?;
        Ok(agreement)
    }

    /// Onboarding requests of the party waiting for DA.
    pub async fn pending_requests(&mut self) -> Result<Vec<String>> {
        let party = self.party;
        let net = self.net;
        Ok(self
            .bridge(T_AGREEMENT_REQUEST)
            .await?
            .into_iter()
            .filter(|(_, v)| text(v, "user").ok() == Some(party) && parties_match(v, net))
            .map(|(cid, _)| cid)
            .collect())
    }

    /// `(cid, reason)` of the party's rejected onboarding requests.
    pub async fn rejected_requests(&mut self) -> Result<Vec<(String, String)>> {
        let party = self.party;
        let net = self.net;
        Ok(self
            .bridge(T_REJECTED_REQUEST)
            .await?
            .into_iter()
            .filter_map(|(cid, v)| {
                let request = v.get("bridgeAgreementOffer")?;
                (text(request, "user").ok() == Some(party) && parties_match(request, net))
                    .then(|| (cid, text(&v, "reason").unwrap_or("").to_string()))
            })
            .collect())
    }

    /// DepositAttestations the party can mint (`_Mint` itself does not check
    /// the recipient, so this filter is the guard).
    pub async fn deposit_attestations(&mut self) -> Result<Vec<DepositAttestation>> {
        let party = self.party;
        let net = self.net;
        let mut out = Vec::new();
        for (cid, v) in self.bridge(T_DEPOSIT_ATTESTATION).await? {
            if text(&v, "recipient").ok() != Some(party) || !parties_match(&v, net) {
                continue;
            }
            out.push(DepositAttestation {
                cid,
                amount: decimal(&v, "amount")?,
                depositor: address_from_word(text(&v, "sourceDepositor").unwrap_or("")),
                reference: text(&v, "reference").unwrap_or("").to_string(),
            });
        }
        Ok(out)
    }

    /// Withdrawals waiting for DA (WithdrawAttestation of the party).
    pub async fn withdraw_attestations(&mut self) -> Result<Vec<Withdrawal>> {
        let party = self.party;
        let net = self.net;
        let mut out = Vec::new();
        for (cid, v) in self.bridge(T_WITHDRAW_ATTESTATION).await? {
            if valid_party(&v, "sourceDepositor").as_deref() == Some(party)
                && parties_match(&v, net)
            {
                out.push(Withdrawal::from_attestation(&cid, &v, None)?);
            }
        }
        Ok(out)
    }

    /// Withdrawals DA accepted (detail = trace id).
    pub async fn accepted_withdrawals(&mut self) -> Result<Vec<Withdrawal>> {
        self.withdraw_results(T_ACCEPTED_WITHDRAW, "traceId").await
    }

    /// Withdrawals DA revoked (detail = reason); their USDCx can be reclaimed.
    pub async fn revoked_withdrawals(&mut self) -> Result<Vec<Withdrawal>> {
        self.withdraw_results(T_REVOKED_WITHDRAW, "reason").await
    }

    async fn withdraw_results(
        &mut self,
        t: (&'static str, &'static str),
        detail_key: &str,
    ) -> Result<Vec<Withdrawal>> {
        let party = self.party;
        let net = self.net;
        let mut out = Vec::new();
        for (cid, v) in self.bridge(t).await? {
            let Some(att) = v.get("attestation") else {
                continue;
            };
            if valid_party(att, "sourceDepositor").as_deref() == Some(party)
                && parties_match(att, net)
            {
                let detail = text(&v, detail_key).ok().map(str::to_string);
                out.push(Withdrawal::from_attestation(&cid, att, detail)?);
            }
        }
        Ok(out)
    }

    /// The party's USDCx holdings.
    pub async fn holdings(&mut self) -> Result<Vec<Holding>> {
        let party = self.party;
        let net = self.net;
        let filter = format!(
            "#utility-registry-holding-v0:{}:{}",
            T_HOLDING.0, T_HOLDING.1
        );
        let mut out = Vec::new();
        for (cid, v) in contracts(self.client, filter, |id| is_template(id, T_HOLDING)).await? {
            if usdcx_holding(&v, party, net) {
                out.push(Holding {
                    cid,
                    amount: decimal(&v, "amount")?,
                    locked: !v.get("lock").is_none_or(Value::is_null),
                });
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Transaction scans
// ---------------------------------------------------------------------------

/// The party's transactions with bridge or holding events after an offset.
pub struct Scan {
    pub transactions: Vec<LedgerTransaction>,
    /// False when the stream ended early: only a prefix of the range was seen.
    pub complete: bool,
}

/// Transactions after `from_exclusive` up to the current ledger end that
/// touch one of `templates` (package-name filters).
pub async fn scan(
    client: &mut DAppProviderClient,
    from_exclusive: i64,
    templates: &[String],
) -> Result<Scan> {
    let end = client.get_ledger_end().await?;
    if end <= from_exclusive {
        return Ok(Scan {
            transactions: vec![],
            complete: true,
        });
    }
    let (updates, complete) = client
        .get_updates(from_exclusive, Some(end), templates)
        .await?;
    let transactions = updates
        .into_iter()
        .filter_map(|u| match u.update {
            Some(get_updates_response::Update::Transaction(t)) => Some(t),
            _ => None,
        })
        .collect();
    Ok(Scan {
        transactions,
        complete,
    })
}

/// The withdrawal templates a burn and its outcome create.
pub fn withdraw_filters() -> Vec<String> {
    vec![
        bridge_filter(T_WITHDRAW_ATTESTATION),
        bridge_filter(T_ACCEPTED_WITHDRAW),
        bridge_filter(T_REVOKED_WITHDRAW),
    ]
}

/// What a scan says about one withdrawal.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct WithdrawalSeen {
    /// The burn's transaction, found by one of its command ids.
    pub burn_update: Option<String>,
    /// A WithdrawAttestation for the request id.
    pub attestation_cid: Option<String>,
    /// DA's Accepted outcome: (cid, trace id).
    pub accepted: Option<(String, Option<String>)>,
    /// DA's Revoked outcome: (cid, reason).
    pub revoked: Option<(String, Option<String>)>,
}

impl WithdrawalSeen {
    pub fn committed(&self) -> bool {
        self.burn_update.is_some()
            || self.attestation_cid.is_some()
            || self.accepted.is_some()
            || self.revoked.is_some()
    }
}

/// Looks for a withdrawal in scanned transactions: its burn by command id, or
/// any bridge contract carrying its request id.
pub fn find_withdrawal(
    transactions: &[LedgerTransaction],
    net: &UsdcxNetwork,
    party: &str,
    request_id: &str,
    command_ids: &[String],
) -> WithdrawalSeen {
    let mut seen = WithdrawalSeen::default();
    for t in transactions {
        let ours = command_ids
            .iter()
            .any(|c| !c.is_empty() && *c == t.command_id);
        for e in &t.events {
            let Some(ledger_event::Event::Created(c)) = &e.event else {
                continue;
            };
            let Some(args) = c.create_arguments.as_ref().map(prost_struct_to_json) else {
                continue;
            };
            if is_bridge(&c.template_id, T_WITHDRAW_ATTESTATION)
                && valid_party(&args, "sourceDepositor").as_deref() == Some(party)
                && parties_match(&args, net)
                && (ours || text(&args, "requestId").ok() == Some(request_id))
            {
                seen.attestation_cid = Some(c.contract_id.clone());
                if ours {
                    seen.burn_update = Some(t.update_id.clone());
                }
            }
            for (template, key, slot) in [
                (T_ACCEPTED_WITHDRAW, "traceId", &mut seen.accepted),
                (T_REVOKED_WITHDRAW, "reason", &mut seen.revoked),
            ] {
                if !is_bridge(&c.template_id, template) {
                    continue;
                }
                let Some(att) = args.get("attestation") else {
                    continue;
                };
                if valid_party(att, "sourceDepositor").as_deref() == Some(party)
                    && parties_match(att, net)
                    && text(att, "requestId").ok() == Some(request_id)
                {
                    *slot = Some((
                        c.contract_id.clone(),
                        text(&args, key).ok().map(str::to_string),
                    ));
                }
            }
        }
        if ours && seen.burn_update.is_none() {
            seen.burn_update = Some(t.update_id.clone());
        }
    }
    seen
}

/// A direct (pre-approved) mint of `amount` to the party after the scan's
/// start: a USDCx holding of that amount created for the party in a
/// transaction this agent did not submit. Returns (update id, holding cid).
pub fn find_direct_mint(
    transactions: &[LedgerTransaction],
    net: &UsdcxNetwork,
    party: &str,
    amount: Decimal,
    skip_updates: &[String],
) -> Option<(String, String)> {
    transactions
        .iter()
        .filter(|t| t.command_id.is_empty() && !skip_updates.contains(&t.update_id))
        .find_map(|t| {
            t.events.iter().find_map(|e| {
                let Some(ledger_event::Event::Created(c)) = &e.event else {
                    return None;
                };
                if !is_template(&c.template_id, T_HOLDING) {
                    return None;
                }
                let args = c.create_arguments.as_ref().map(prost_struct_to_json)?;
                (usdcx_holding(&args, party, net) && decimal(&args, "amount").ok() == Some(amount))
                    .then(|| (t.update_id.clone(), c.contract_id.clone()))
            })
        })
}

/// The holding filter, for direct-mint scans.
pub fn holding_filters() -> Vec<String> {
    vec![format!(
        "#utility-registry-holding-v0:{}:{}",
        T_HOLDING.0, T_HOLDING.1
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tx_verifier::usdcx::MAINNET;

    const PARTY: &str =
        "tester::12205e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";

    fn da(mut v: Value) -> Value {
        v["crossChainRepresentative"] = json!(MAINNET.admin);
        v["bridgeOperator"] = json!(MAINNET.bridge_operator);
        v["operator"] = json!(MAINNET.utility_operator);
        v["instrumentId"] = json!({"admin": MAINNET.admin, "id": "USDCx"});
        v
    }

    #[test]
    fn bridge_templates_must_come_from_an_allowed_package() {
        let ok = format!(
            "{}:Utility.Bridge.V0.Agreement.User:BridgeUserAgreement",
            BRIDGE_PACKAGE_IDS[0]
        );
        assert!(is_bridge(&ok, T_AGREEMENT));
        assert!(!is_bridge(
            "feed:Utility.Bridge.V0.Agreement.User:BridgeUserAgreement",
            T_AGREEMENT
        ));
        assert!(!is_bridge(&ok, T_AGREEMENT_REQUEST));
    }

    #[test]
    fn int64_reads_as_number_float_or_string() {
        let v = json!({"a": 10001, "b": 10001.0, "c": "10001", "d": 1.5});
        assert_eq!(int(&v, "a").unwrap(), 10001);
        assert_eq!(int(&v, "b").unwrap(), 10001);
        assert_eq!(int(&v, "c").unwrap(), 10001);
        assert!(int(&v, "d").is_err());
    }

    #[test]
    fn da_parties_and_the_pinned_instrument_are_required() {
        let v = da(json!({}));
        assert!(parties_match(&v, &MAINNET));
        let mut wrong = v.clone();
        wrong["bridgeOperator"] = json!("someone::1220aa");
        assert!(!parties_match(&wrong, &MAINNET));
        let mut wrong = v;
        wrong["instrumentId"]["id"] = json!("USDC");
        assert!(!parties_match(&wrong, &MAINNET));
    }

    #[test]
    fn valid_party_reads_both_variant_encodings() {
        let v = json!({"a": {"tag": "Valid", "value": PARTY}, "b": {"constructor": "Valid", "value": PARTY}, "c": {"tag": "Invalid", "value": "00"}});
        assert_eq!(valid_party(&v, "a").as_deref(), Some(PARTY));
        assert_eq!(valid_party(&v, "b").as_deref(), Some(PARTY));
        assert_eq!(valid_party(&v, "c"), None);
    }

    #[test]
    fn usdcx_holdings_need_the_pinned_registrar_and_scheme() {
        let h = json!({
            "owner": PARTY,
            "registrar": MAINNET.admin,
            "instrument": {"source": MAINNET.admin, "id": "USDCx", "scheme": "RegistrarInternalScheme"},
            "amount": "5.0",
            "lock": null,
        });
        assert!(usdcx_holding(&h, PARTY, &MAINNET));
        let mut fake = h.clone();
        fake["registrar"] = json!("fake::1220aa");
        assert!(!usdcx_holding(&fake, PARTY, &MAINNET));
        let mut other = h;
        other["instrument"]["scheme"] = json!("Other");
        assert!(!usdcx_holding(&other, PARTY, &MAINNET));
    }
}
