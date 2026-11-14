//! Inspection of the typed USDCx bridge operations. Every check rejects.
//!
//! The transaction must have exactly one root, act as the party alone on the
//! pinned synchronizer with the expected command id, exercise or create a
//! `utility-bridge-v0` template of an allowed package, and touch only
//! contracts signed by the parties pinned for the network: a look-alike
//! template in another package lacks their signatures. Amounts compare as
//! exact decimals.

use std::str::FromStr;

use anyhow::{Context, Result};
use prost::Message;
use rust_decimal::Decimal;

use crate::decode::canton_proto::com::daml::ledger::api::v2 as proto_v2;
use crate::inspector::{
    as_list, as_record, get_contract_id, get_node_type, get_numeric, get_party, get_record_field,
    get_text, node_summary,
};
use crate::types::{InspectionResult, OperationExpectation};
use crate::usdcx::{
    BRIDGE_PACKAGE_IDS, CHOICE_BURN, CHOICE_MINT, CHOICE_RECLAIM, CHOICE_TERMINATE,
    CHOICE_UPDATE_PRE_APPROVAL, ETHEREUM_DOMAIN, HOLDING_SCHEME, INSTRUMENT_ID, T_AGREEMENT,
    T_AGREEMENT_REQUEST, T_ALLOCATION_FACTORY, T_DEPOSIT_ATTESTATION, T_HOLDING,
    T_INSTRUMENT_CONFIGURATION, T_REVOKED_WITHDRAW, T_WITHDRAW_ATTESTATION, UsdcxNetwork,
};
use proto_v2::interactive::metadata::input_contract::Contract as InputContract;
use proto_v2::interactive::transaction::v1::node::NodeType;
use proto_v2::interactive::transaction::v1::{Create, Exercise};
use proto_v2::interactive::{DamlTransaction, Metadata, PreparedTransaction};
use proto_v2::{Identifier, Record, Value, value::Sum};

/// A failed check, as the rejection reason.
type Check<T = ()> = std::result::Result<T, String>;

/// The outcome of a USDCx inspection.
pub(crate) struct UsdcxInspection {
    pub result: InspectionResult,
    /// The holdings a burn spends (empty for the other operations).
    pub burn_holding_cids: Vec<String>,
}

struct Verified {
    summary: String,
    burn_holding_cids: Vec<String>,
}

/// Inspects a USDCx operation; `None` when `expectation` is not one.
pub(crate) fn inspect(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
) -> Result<Option<UsdcxInspection>> {
    let (op, network) = match expectation {
        OperationExpectation::UsdcxRequestAgreement { network, .. } => {
            ("UsdcxRequestAgreement", network)
        }
        OperationExpectation::UsdcxMint { network, .. } => ("UsdcxMint", network),
        OperationExpectation::UsdcxBurn { network, .. } => ("UsdcxBurn", network),
        OperationExpectation::UsdcxReclaimHolding { network, .. } => {
            ("UsdcxReclaimHolding", network)
        }
        OperationExpectation::UsdcxUpdatePreApproval { network, .. } => {
            ("UsdcxUpdatePreApproval", network)
        }
        OperationExpectation::UsdcxTerminateAgreement { network, .. } => {
            ("UsdcxTerminateAgreement", network)
        }
        _ => return Ok(None),
    };
    let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
        .context("Failed to decode PreparedTransaction protobuf")?;
    let outcome = UsdcxNetwork::by_name(network)
        .map_err(|e| format!("{e:#}"))
        .and_then(|net| check(&prepared, expectation, net));
    Ok(Some(match outcome {
        Ok(v) => UsdcxInspection {
            result: InspectionResult {
                accepted: true,
                summary: format!("{op} VERIFIED: {}", v.summary),
                warnings: vec![],
                rejection_reason: None,
            },
            burn_holding_cids: v.burn_holding_cids,
        },
        Err(reason) => UsdcxInspection {
            result: InspectionResult {
                accepted: false,
                summary: format!("{op} REJECTED: {reason}"),
                warnings: vec![],
                rejection_reason: Some(reason),
            },
            burn_holding_cids: vec![],
        },
    }))
}

fn check(
    p: &PreparedTransaction,
    expectation: &OperationExpectation,
    net: &UsdcxNetwork,
) -> Check<Verified> {
    match expectation {
        OperationExpectation::UsdcxRequestAgreement {
            party,
            pre_approval,
            command_id,
            ..
        } => request_agreement(p, net, party, *pre_approval, command_id),
        OperationExpectation::UsdcxMint {
            party,
            deposit_attestation_cid,
            command_id,
            ..
        } => mint(p, net, party, deposit_attestation_cid, command_id),
        OperationExpectation::UsdcxBurn {
            party,
            amount,
            destination_recipient,
            request_id,
            command_id,
            ..
        } => burn(
            p,
            net,
            party,
            amount,
            destination_recipient,
            request_id,
            command_id,
        ),
        OperationExpectation::UsdcxReclaimHolding {
            party,
            revoked_withdraw_attestation_cid,
            command_id,
            ..
        } => reclaim(p, net, party, revoked_withdraw_attestation_cid, command_id),
        OperationExpectation::UsdcxUpdatePreApproval {
            party,
            pre_approval,
            ..
        } => update_pre_approval(p, net, party, *pre_approval),
        OperationExpectation::UsdcxTerminateAgreement { party, reason, .. } => {
            terminate(p, net, party, reason)
        }
        _ => Err("not a USDCx operation".to_string()),
    }
}

// ============================================================================
// Operations
// ============================================================================

fn request_agreement(
    p: &PreparedTransaction,
    net: &UsdcxNetwork,
    party: &str,
    pre_approval: bool,
    command_id: &str,
) -> Check<Verified> {
    let parts = parts(p, net, party, Some(command_id))?;
    let NodeType::Create(create) = parts.root else {
        return Err("the root is not a Create".to_string());
    };
    ensure(parts.tx.nodes.len() == 1, || {
        format!(
            "expected a single Create node, got {} nodes",
            parts.tx.nodes.len()
        )
    })?;
    bridge_template(
        create.template_id.as_ref(),
        T_AGREEMENT_REQUEST,
        "created contract",
    )?;
    let args = create_record(create, "BridgeUserAgreementRequest")?;
    da_parties(args, net, "BridgeUserAgreementRequest")?;
    expect_party(args, "user", party, "BridgeUserAgreementRequest")?;
    let requested = bool_field(args, "preApproval")?;
    ensure(requested == pre_approval, || {
        format!("preApproval is {requested}, expected {pre_approval}")
    })?;
    signed_by(&create.signatories, &[party], "BridgeUserAgreementRequest")?;
    Ok(Verified {
        summary: format!(
            "{party} requests a BridgeUserAgreement (preApproval {pre_approval}) | {}",
            node_summary(parts.tx)
        ),
        burn_holding_cids: vec![],
    })
}

fn mint(
    p: &PreparedTransaction,
    net: &UsdcxNetwork,
    party: &str,
    attestation_cid: &str,
    command_id: &str,
) -> Check<Verified> {
    let parts = parts(p, net, party, Some(command_id))?;
    let (_, chosen) = agreement_exercise(&parts, net, party, CHOICE_MINT)?;
    let cid = cid_field(chosen, "depositAttestationCid")?;
    ensure(cid == attestation_cid, || {
        format!("depositAttestationCid is {cid}, expected {attestation_cid}")
    })?;
    let attestation = input_contract(parts.metadata, cid, "DepositAttestation")?;
    bridge_template(
        attestation.template_id.as_ref(),
        T_DEPOSIT_ATTESTATION,
        "DepositAttestation",
    )?;
    let args = create_record(attestation, "DepositAttestation")?;
    expect_party(args, "recipient", party, "DepositAttestation")?;
    da_parties(args, net, "DepositAttestation")?;
    signed_by(
        &attestation.signatories,
        &[net.admin, net.bridge_operator],
        "DepositAttestation",
    )?;
    let amount = numeric_field(args, "amount")?;
    factory_and_context(chosen, parts.metadata, net)?;
    Ok(Verified {
        summary: format!(
            "{party} mints {amount} USDCx from DepositAttestation {attestation_cid} | {}",
            node_summary(parts.tx)
        ),
        burn_holding_cids: vec![],
    })
}

#[allow(clippy::too_many_arguments)]
fn burn(
    p: &PreparedTransaction,
    net: &UsdcxNetwork,
    party: &str,
    amount: &str,
    recipient: &str,
    request_id: &str,
    command_id: &str,
) -> Check<Verified> {
    ensure(is_lower_eth_address(recipient), || {
        format!("the expected recipient {recipient} is not 0x + 40 lowercase hex")
    })?;
    let parts = parts(p, net, party, Some(command_id))?;
    let (_, chosen) = agreement_exercise(&parts, net, party, CHOICE_BURN)?;
    decimal_eq(numeric_field(chosen, "amount")?, amount, "burn amount")?;
    expect_int(chosen, "destinationDomain", ETHEREUM_DOMAIN)?;
    expect_text(chosen, "destinationRecipient", recipient)?;
    expect_text(chosen, "requestId", request_id)?;
    expect_text(chosen, "reference", "")?;

    let holdings = list_field(chosen, "holdingCids")?;
    ensure(!holdings.is_empty(), || {
        "the burn spends no holdings".to_string()
    })?;
    let mut cids: Vec<String> = Vec::with_capacity(holdings.len());
    for h in holdings {
        let cid = get_contract_id(h).ok_or("holdingCids holds a non contract id")?;
        ensure(!cids.iter().any(|c| c == cid), || {
            format!("holding {cid} is listed twice")
        })?;
        let holding = input_contract(parts.metadata, cid, "input holding")?;
        usdcx_holding(holding, net, party)?;
        cids.push(cid.to_string());
    }
    factory_and_context(chosen, parts.metadata, net)?;

    // The burn's WithdrawAttestation: exactly one, carrying the same terms.
    let attestations: Vec<&Create> = creates(parts.tx)
        .filter(|c| {
            c.template_id.as_ref().is_some_and(|t| {
                t.module_name == T_WITHDRAW_ATTESTATION.0
                    && t.entity_name == T_WITHDRAW_ATTESTATION.1
            })
        })
        .collect();
    let [attestation] = attestations.as_slice() else {
        return Err(format!(
            "expected exactly one WithdrawAttestation create, got {}",
            attestations.len()
        ));
    };
    bridge_template(
        attestation.template_id.as_ref(),
        T_WITHDRAW_ATTESTATION,
        "WithdrawAttestation",
    )?;
    let att = create_record(attestation, "WithdrawAttestation")?;
    decimal_eq(
        numeric_field(att, "amount")?,
        amount,
        "WithdrawAttestation amount",
    )?;
    expect_int(att, "destinationDomain", ETHEREUM_DOMAIN)?;
    expect_text(att, "destinationRecipient", recipient)?;
    expect_text(att, "requestId", request_id)?;
    if get_record_field(att, "reference").is_some() {
        expect_text(att, "reference", "")?;
    }
    let depositor = valid_party(att, "sourceDepositor")?;
    ensure(depositor == party, || {
        format!("WithdrawAttestation sourceDepositor is {depositor}, expected {party}")
    })?;
    da_parties(att, net, "WithdrawAttestation")?;
    signed_by(
        &attestation.signatories,
        &[net.admin, net.bridge_operator, party],
        "WithdrawAttestation",
    )?;

    Ok(Verified {
        summary: format!(
            "{party} burns {amount} USDCx to {recipient} on Ethereum (request {request_id}, \
             {} input holding(s)) | {}",
            cids.len(),
            node_summary(parts.tx)
        ),
        burn_holding_cids: cids,
    })
}

fn reclaim(
    p: &PreparedTransaction,
    net: &UsdcxNetwork,
    party: &str,
    revoked_cid: &str,
    command_id: &str,
) -> Check<Verified> {
    let parts = parts(p, net, party, Some(command_id))?;
    let (_, chosen) = agreement_exercise(&parts, net, party, CHOICE_RECLAIM)?;
    let cid = cid_field(chosen, "revokedWithdrawAttestationCid")?;
    ensure(cid == revoked_cid, || {
        format!("revokedWithdrawAttestationCid is {cid}, expected {revoked_cid}")
    })?;
    let revoked = input_contract(parts.metadata, cid, "RevokedWithdrawAttestation")?;
    bridge_template(
        revoked.template_id.as_ref(),
        T_REVOKED_WITHDRAW,
        "RevokedWithdrawAttestation",
    )?;
    let args = create_record(revoked, "RevokedWithdrawAttestation")?;
    let att = record_field(args, "attestation")?;
    let depositor = valid_party(att, "sourceDepositor")?;
    ensure(depositor == party, || {
        format!("the revoked withdrawal's sourceDepositor is {depositor}, expected {party}")
    })?;
    da_parties(att, net, "RevokedWithdrawAttestation")?;
    signed_by(
        &revoked.signatories,
        &[net.admin, net.bridge_operator],
        "RevokedWithdrawAttestation",
    )?;
    let amount = numeric_field(att, "amount")?;
    factory_and_context(chosen, parts.metadata, net)?;
    Ok(Verified {
        summary: format!(
            "{party} reclaims {amount} USDCx of revoked withdrawal {revoked_cid} | {}",
            node_summary(parts.tx)
        ),
        burn_holding_cids: vec![],
    })
}

fn update_pre_approval(
    p: &PreparedTransaction,
    net: &UsdcxNetwork,
    party: &str,
    pre_approval: bool,
) -> Check<Verified> {
    let parts = parts(p, net, party, None)?;
    let (_, chosen) = agreement_exercise(&parts, net, party, CHOICE_UPDATE_PRE_APPROVAL)?;
    let requested = bool_field(chosen, "preApproval")?;
    ensure(requested == pre_approval, || {
        format!("preApproval is {requested}, expected {pre_approval}")
    })?;
    Ok(Verified {
        summary: format!(
            "{party} sets preApproval to {pre_approval} | {}",
            node_summary(parts.tx)
        ),
        burn_holding_cids: vec![],
    })
}

fn terminate(
    p: &PreparedTransaction,
    net: &UsdcxNetwork,
    party: &str,
    reason: &str,
) -> Check<Verified> {
    let parts = parts(p, net, party, None)?;
    let (_, chosen) = agreement_exercise(&parts, net, party, CHOICE_TERMINATE)?;
    expect_party(chosen, "actor", party, "Terminate")?;
    expect_text(chosen, "reason", reason)?;
    Ok(Verified {
        summary: format!(
            "{party} terminates its BridgeUserAgreement | {}",
            node_summary(parts.tx)
        ),
        burn_holding_cids: vec![],
    })
}

// ============================================================================
// Shared checks
// ============================================================================

struct Parts<'a> {
    tx: &'a DamlTransaction,
    metadata: &'a Metadata,
    root: &'a NodeType,
}

/// One root, `act_as == [party]`, the pinned synchronizer, no key mapping and,
/// when given, the expected command id.
fn parts<'a>(
    p: &'a PreparedTransaction,
    net: &UsdcxNetwork,
    party: &str,
    command_id: Option<&str>,
) -> Check<Parts<'a>> {
    let tx = p.transaction.as_ref().ok_or("missing transaction")?;
    let metadata = p.metadata.as_ref().ok_or("missing metadata")?;
    let submitter = metadata
        .submitter_info
        .as_ref()
        .ok_or("missing submitter_info")?;
    ensure(submitter.act_as == [party], || {
        format!(
            "act_as is {:?}, expected exactly [{party}]",
            submitter.act_as
        )
    })?;
    if let Some(expected) = command_id {
        ensure(submitter.command_id == expected, || {
            format!(
                "command_id is {}, expected {expected}",
                submitter.command_id
            )
        })?;
    }
    ensure(metadata.synchronizer_id == net.synchronizer_id, || {
        format!(
            "synchronizer is {}, expected {} ({})",
            metadata.synchronizer_id, net.synchronizer_id, net.name
        )
    })?;
    ensure(metadata.global_key_mapping.is_empty(), || {
        "the transaction carries a global key mapping".to_string()
    })?;
    let [root_id] = tx.roots.as_slice() else {
        return Err(format!(
            "expected exactly one root node, got {}",
            tx.roots.len()
        ));
    };
    let root = tx
        .nodes
        .iter()
        .find(|n| n.node_id == *root_id)
        .and_then(get_node_type)
        .ok_or("the root node is missing")?;
    Ok(Parts { tx, metadata, root })
}

/// The root exercise of `choice` by the party alone on its own agreement
/// (signed by the USDCx admin, the bridge operator and the party), and the
/// choice argument.
fn agreement_exercise<'a>(
    parts: &Parts<'a>,
    net: &UsdcxNetwork,
    party: &str,
    choice: &str,
) -> Check<(&'a Exercise, &'a Record)> {
    let NodeType::Exercise(ex) = parts.root else {
        return Err("the root is not an Exercise".to_string());
    };
    bridge_template(ex.template_id.as_ref(), T_AGREEMENT, "exercised contract")?;
    ensure(ex.interface_id.is_none(), || {
        "the choice is exercised through an interface".to_string()
    })?;
    ensure(ex.choice_id == choice, || {
        format!("choice is {}, expected {choice}", ex.choice_id)
    })?;
    ensure(ex.acting_parties == [party], || {
        format!(
            "acting parties are {:?}, expected exactly [{party}]",
            ex.acting_parties
        )
    })?;
    let agreement = input_contract(parts.metadata, &ex.contract_id, "BridgeUserAgreement")?;
    bridge_template(
        agreement.template_id.as_ref(),
        T_AGREEMENT,
        "BridgeUserAgreement",
    )?;
    let args = create_record(agreement, "BridgeUserAgreement")?;
    expect_party(args, "user", party, "BridgeUserAgreement")?;
    da_parties(args, net, "BridgeUserAgreement")?;
    signed_by(
        &agreement.signatories,
        &[net.admin, net.bridge_operator, party],
        "BridgeUserAgreement",
    )?;
    let chosen = ex
        .chosen_value
        .as_ref()
        .and_then(as_record)
        .ok_or("the choice argument is not a record")?;
    Ok((ex, chosen))
}

/// The factory and the choice context of a mint, burn or reclaim: DA's
/// AllocationFactory and the USDCx InstrumentConfiguration, no app-reward
/// configuration and no featured-app right (they route app rewards).
fn factory_and_context(chosen: &Record, metadata: &Metadata, net: &UsdcxNetwork) -> Check {
    let factory_cid = cid_field(chosen, "factoryCid")?;
    let factory = input_contract(metadata, factory_cid, "factory")?;
    template(
        factory.template_id.as_ref(),
        T_ALLOCATION_FACTORY,
        "factory",
    )?;
    let args = create_record(factory, "factory")?;
    expect_party(args, "registrar", net.admin, "factory")?;
    expect_party(args, "operator", net.utility_operator, "factory")?;
    signed_by(&factory.signatories, &[net.admin], "factory")?;

    let context = record_field(chosen, "contextContractIds")?;
    let config_cid = cid_field(context, "instrumentConfigurationCid")?;
    let config = input_contract(metadata, config_cid, "instrument configuration")?;
    template(
        config.template_id.as_ref(),
        T_INSTRUMENT_CONFIGURATION,
        "instrument configuration",
    )?;
    let config_args = create_record(config, "instrument configuration")?;
    expect_party(
        config_args,
        "registrar",
        net.admin,
        "instrument configuration",
    )?;
    signed_by(
        &config.signatories,
        &[net.admin],
        "instrument configuration",
    )?;
    none_or_absent(context, "appRewardConfigurationCid")?;
    none_or_absent(context, "featuredAppRightCid")?;
    Ok(())
}

/// An unlocked USDCx holding of the party, signed by the registrar and the party.
fn usdcx_holding(h: &Create, net: &UsdcxNetwork, party: &str) -> Check {
    template(h.template_id.as_ref(), T_HOLDING, "input holding")?;
    let args = create_record(h, "input holding")?;
    expect_party(args, "owner", party, "input holding")?;
    expect_party(args, "registrar", net.admin, "input holding")?;
    let instrument = record_field(args, "instrument")?;
    expect_party(instrument, "source", net.admin, "input holding instrument")?;
    expect_text(instrument, "id", INSTRUMENT_ID)?;
    expect_text(instrument, "scheme", HOLDING_SCHEME)?;
    none_or_absent(args, "lock")?;
    signed_by(&h.signatories, &[net.admin, party], "input holding")
}

/// The DA parties and the USDCx instrument of a bridge contract.
fn da_parties(r: &Record, net: &UsdcxNetwork, what: &str) -> Check {
    expect_party(r, "crossChainRepresentative", net.admin, what)?;
    expect_party(r, "bridgeOperator", net.bridge_operator, what)?;
    expect_party(r, "operator", net.utility_operator, what)?;
    let instrument = record_field(r, "instrumentId")?;
    expect_party(instrument, "admin", net.admin, what)?;
    expect_text(instrument, "id", INSTRUMENT_ID)
}

// ============================================================================
// Helpers
// ============================================================================

fn ensure(condition: bool, reason: impl FnOnce() -> String) -> Check {
    if condition { Ok(()) } else { Err(reason()) }
}

/// A `utility-bridge-v0` template of an allowed package.
fn bridge_template(id: Option<&Identifier>, expected: (&str, &str), what: &str) -> Check {
    let id = template(id, expected, what)?;
    ensure(BRIDGE_PACKAGE_IDS.contains(&id.package_id.as_str()), || {
        format!(
            "{what} uses package {}, which is not an allowed utility-bridge-v0 version",
            id.package_id
        )
    })
}

/// The template (module and entity) of a contract.
fn template<'a>(
    id: Option<&'a Identifier>,
    (module, entity): (&str, &str),
    what: &str,
) -> Check<&'a Identifier> {
    let id = id.ok_or_else(|| format!("{what} has no template id"))?;
    ensure(id.module_name == module && id.entity_name == entity, || {
        format!(
            "{what} is {}:{}, expected {module}:{entity}",
            id.module_name, id.entity_name
        )
    })?;
    Ok(id)
}

fn signed_by(signatories: &[String], required: &[&str], what: &str) -> Check {
    for party in required {
        ensure(signatories.iter().any(|s| s == party), || {
            format!("{what} is not signed by {party}")
        })?;
    }
    Ok(())
}

/// An input contract of the transaction, by contract id.
fn input_contract<'a>(metadata: &'a Metadata, cid: &str, what: &str) -> Check<&'a Create> {
    metadata
        .input_contracts
        .iter()
        .find_map(|ic| match &ic.contract {
            Some(InputContract::V1(c)) if c.contract_id == cid => Some(c),
            _ => None,
        })
        .ok_or_else(|| format!("{what} {cid} is not among the input contracts"))
}

fn create_record<'a>(c: &'a Create, what: &str) -> Check<&'a Record> {
    c.argument
        .as_ref()
        .and_then(as_record)
        .ok_or_else(|| format!("{what} has no record argument"))
}

/// Every Create node of the transaction.
fn creates(tx: &DamlTransaction) -> impl Iterator<Item = &Create> {
    tx.nodes.iter().filter_map(|n| match get_node_type(n) {
        Some(NodeType::Create(c)) => Some(c),
        _ => None,
    })
}

fn field<'a>(r: &'a Record, label: &str) -> Check<&'a Value> {
    get_record_field(r, label).ok_or_else(|| format!("missing field {label}"))
}

fn record_field<'a>(r: &'a Record, label: &str) -> Check<&'a Record> {
    as_record(field(r, label)?).ok_or_else(|| format!("{label} is not a record"))
}

fn list_field<'a>(r: &'a Record, label: &str) -> Check<&'a [Value]> {
    as_list(field(r, label)?).ok_or_else(|| format!("{label} is not a list"))
}

fn numeric_field<'a>(r: &'a Record, label: &str) -> Check<&'a str> {
    get_numeric(field(r, label)?).ok_or_else(|| format!("{label} is not a numeric"))
}

fn cid_field<'a>(r: &'a Record, label: &str) -> Check<&'a str> {
    get_contract_id(field(r, label)?).ok_or_else(|| format!("{label} is not a contract id"))
}

fn bool_field(r: &Record, label: &str) -> Check<bool> {
    match field(r, label)?.sum.as_ref() {
        Some(Sum::Bool(b)) => Ok(*b),
        _ => Err(format!("{label} is not a Bool")),
    }
}

fn expect_party(r: &Record, label: &str, expected: &str, what: &str) -> Check {
    let got =
        get_party(field(r, label)?).ok_or_else(|| format!("{what} {label} is not a party"))?;
    ensure(got == expected, || {
        format!("{what} {label} is {got}, expected {expected}")
    })
}

fn expect_text(r: &Record, label: &str, expected: &str) -> Check {
    let got = get_text(field(r, label)?).ok_or_else(|| format!("{label} is not a Text"))?;
    ensure(got == expected, || {
        format!("{label} is {got:?}, expected {expected:?}")
    })
}

fn expect_int(r: &Record, label: &str, expected: i64) -> Check {
    match field(r, label)?.sum.as_ref() {
        Some(Sum::Int64(n)) if *n == expected => Ok(()),
        Some(Sum::Int64(n)) => Err(format!("{label} is {n}, expected {expected}")),
        _ => Err(format!("{label} is not an Int64")),
    }
}

/// A `None` optional, or the field left out (a record's trailing `None`
/// fields may be omitted).
fn none_or_absent(r: &Record, label: &str) -> Check {
    match get_record_field(r, label).map(|v| v.sum.as_ref()) {
        None => Ok(()),
        Some(Some(Sum::Optional(o))) if o.value.is_none() => Ok(()),
        _ => Err(format!("{label} must be None")),
    }
}

/// The party of a `ValidParty` variant (`Valid party`).
fn valid_party<'a>(r: &'a Record, label: &str) -> Check<&'a str> {
    match field(r, label)?.sum.as_ref() {
        Some(Sum::Variant(v)) if v.constructor == "Valid" => v
            .value
            .as_deref()
            .and_then(get_party)
            .ok_or_else(|| format!("{label} holds no party")),
        _ => Err(format!("{label} is not a Valid party")),
    }
}

/// Exact decimal equality ("10.5" equals "10.5000000000").
fn decimal_eq(actual: &str, expected: &str, what: &str) -> Check {
    let a =
        Decimal::from_str(actual).map_err(|e| format!("{what} {actual} is not a decimal: {e}"))?;
    let b = Decimal::from_str(expected)
        .map_err(|e| format!("expected {what} {expected} is not a decimal: {e}"))?;
    ensure(a == b, || {
        format!("{what} is {actual}, expected {expected}")
    })
}

/// `0x` + 40 lowercase hex characters.
fn is_lower_eth_address(s: &str) -> bool {
    s.strip_prefix("0x").is_some_and(|hex| {
        hex.len() == 40
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use crate::usdcx::{self, MAINNET};
    use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
    use proto_v2::{RecordField, Variant};

    const PARTY: &str =
        "tester::12205e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";
    const OTHER: &str =
        "other::12207a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a";
    const AGREEMENT: &str = "00a1";
    const FACTORY: &str = "00f1";
    const CONFIG: &str = "00c1";
    const H1: &str = "00b1";
    const H2: &str = "00b2";
    const DEPOSIT: &str = "00d1";
    const REVOKED: &str = "00e1";
    const REQUEST: &str = "6f1c2c7e-4b8a-4c41-9a3e-2d5b7f0c9e11";
    const TO: &str = "0x1234567890abcdef1234567890abcdef12345678";
    const NET: &UsdcxNetwork = &MAINNET;

    fn bridge(t: (&str, &str)) -> Identifier {
        Identifier {
            package_id: BRIDGE_PACKAGE_IDS[1].into(),
            module_name: t.0.into(),
            entity_name: t.1.into(),
        }
    }

    fn registry(t: (&str, &str)) -> Identifier {
        Identifier {
            package_id: "registry".into(),
            module_name: t.0.into(),
            entity_name: t.1.into(),
        }
    }

    fn int64(n: i64) -> Value {
        Value {
            sum: Some(Sum::Int64(n)),
        }
    }

    fn valid(p: &str) -> Value {
        Value {
            sum: Some(Sum::Variant(Box::new(Variant {
                variant_id: None,
                constructor: "Valid".into(),
                value: Some(Box::new(party(p))),
            }))),
        }
    }

    fn da_fields() -> Vec<(&'static str, Value)> {
        vec![
            ("crossChainRepresentative", party(NET.admin)),
            ("operator", party(NET.utility_operator)),
            ("bridgeOperator", party(NET.bridge_operator)),
            (
                "instrumentId",
                record(vec![("admin", party(NET.admin)), ("id", text("USDCx"))]),
            ),
        ]
    }

    fn with_cid(mut c: Create, cid: &str) -> Create {
        c.contract_id = cid.into();
        c
    }

    fn agreement() -> Create {
        let mut f = da_fields();
        f.push(("user", party(PARTY)));
        f.push(("preApproval", boolean(false)));
        with_cid(
            create(
                bridge(T_AGREEMENT),
                record(f),
                &[NET.admin, NET.bridge_operator, PARTY],
            ),
            AGREEMENT,
        )
    }

    fn holding(cid: &str, owner: &str) -> Create {
        let args = record(vec![
            ("operator", party(NET.utility_operator)),
            ("provider", party(NET.admin)),
            ("registrar", party(NET.admin)),
            ("owner", party(owner)),
            (
                "instrument",
                record(vec![
                    ("source", party(NET.admin)),
                    ("id", text("USDCx")),
                    ("scheme", text("RegistrarInternalScheme")),
                ]),
            ),
            ("label", text("")),
            ("amount", numeric("6.0000000000")),
            ("lock", optional(None)),
        ]);
        with_cid(create(registry(T_HOLDING), args, &[NET.admin, owner]), cid)
    }

    fn factory() -> Create {
        let args = record(vec![
            ("provider", party(NET.admin)),
            ("registrar", party(NET.admin)),
            ("operator", party(NET.utility_operator)),
        ]);
        with_cid(
            create(registry(T_ALLOCATION_FACTORY), args, &[NET.admin]),
            FACTORY,
        )
    }

    fn config() -> Create {
        let args = record(vec![
            ("operator", party(NET.utility_operator)),
            ("provider", party(NET.admin)),
            ("registrar", party(NET.admin)),
        ]);
        with_cid(
            create(registry(T_INSTRUMENT_CONFIGURATION), args, &[NET.admin]),
            CONFIG,
        )
    }

    fn context() -> Value {
        record(vec![
            ("instrumentConfigurationCid", contract_id(CONFIG)),
            ("appRewardConfigurationCid", optional(None)),
            ("featuredAppRightCid", optional(None)),
        ])
    }

    fn agreement_exercise(choice: &str, chosen: Value, children: Vec<String>) -> Exercise {
        Exercise {
            contract_id: AGREEMENT.into(),
            template_id: Some(bridge(T_AGREEMENT)),
            choice_id: choice.into(),
            chosen_value: Some(chosen),
            acting_parties: vec![PARTY.into()],
            consuming: false,
            children,
            ..exercise("x", &[], vec![])
        }
    }

    fn finish(
        mut p: PreparedTransaction,
        command_id: &str,
        inputs: Vec<Create>,
    ) -> PreparedTransaction {
        let m = p.metadata.as_mut().unwrap();
        m.synchronizer_id = NET.synchronizer_id.into();
        m.submitter_info.as_mut().unwrap().command_id = command_id.into();
        inputs.into_iter().fold(p, with_input_contract)
    }

    fn burn_args() -> Value {
        record(vec![
            ("amount", numeric("10.5000000000")),
            ("destinationDomain", int64(0)),
            ("destinationRecipient", text(TO)),
            ("holdingCids", list(vec![contract_id(H1), contract_id(H2)])),
            ("requestId", text(REQUEST)),
            ("reference", text("")),
            ("factoryCid", contract_id(FACTORY)),
            ("contextContractIds", context()),
        ])
    }

    fn withdraw_attestation() -> Create {
        let mut f = da_fields();
        f.extend([
            ("sourceDepositor", valid(PARTY)),
            ("requestId", text(REQUEST)),
            ("amount", numeric("10.5000000000")),
            ("destinationDomain", int64(0)),
            ("destinationRecipient", text(TO)),
            ("reference", text("")),
        ]);
        with_cid(
            create(
                bridge(T_WITHDRAW_ATTESTATION),
                record(f),
                &[NET.admin, NET.bridge_operator, PARTY],
            ),
            "00d9",
        )
    }

    fn burn_tx() -> PreparedTransaction {
        let root = exercise_node(
            "0",
            agreement_exercise(CHOICE_BURN, burn_args(), ids(&["1"])),
        );
        let att = create_node("1", withdraw_attestation());
        finish(
            prepared(&["0"], vec![root, att], vec![seed(0, 1)], &[PARTY]),
            &usdcx::burn_command_id(REQUEST),
            vec![
                agreement(),
                holding(H1, PARTY),
                holding(H2, PARTY),
                factory(),
                config(),
            ],
        )
    }

    fn burn_expectation() -> OperationExpectation {
        OperationExpectation::UsdcxBurn {
            party: PARTY.into(),
            network: "mainnet".into(),
            amount: "10.5".into(),
            destination_recipient: TO.into(),
            request_id: REQUEST.into(),
            command_id: usdcx::burn_command_id(REQUEST),
        }
    }

    fn run(p: &PreparedTransaction, e: &OperationExpectation) -> UsdcxInspection {
        inspect(&encode(p), e).unwrap().unwrap()
    }

    fn accepted(p: &PreparedTransaction, e: &OperationExpectation) -> UsdcxInspection {
        let i = run(p, e);
        assert!(i.result.accepted, "{:?}", i.result.rejection_reason);
        i
    }

    #[track_caller]
    fn rejected(p: &PreparedTransaction, e: &OperationExpectation, needle: &str) {
        let r = run(p, e).result;
        assert!(
            !r.accepted,
            "accepted, expected a rejection containing {needle:?}"
        );
        let reason = r.rejection_reason.unwrap();
        assert!(
            reason.contains(needle),
            "{reason:?} does not contain {needle:?}"
        );
    }

    // --- mutation helpers ---

    fn root_mut(p: &mut PreparedTransaction) -> &mut NodeType {
        let node = p
            .transaction
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|n| n.node_id == "0");
        match node.unwrap().versioned_node.as_mut().unwrap() {
            proto_v2::interactive::daml_transaction::node::VersionedNode::V1(n) => {
                n.node_type.as_mut().unwrap()
            }
        }
    }

    fn exercise_mut(p: &mut PreparedTransaction) -> &mut Exercise {
        match root_mut(p) {
            NodeType::Exercise(ex) => ex,
            _ => panic!("root is not an exercise"),
        }
    }

    fn record_mut(v: &mut Value) -> &mut Record {
        match v.sum.as_mut().unwrap() {
            Sum::Record(r) => r,
            _ => panic!("not a record"),
        }
    }

    fn set(r: &mut Record, label: &str, value: Value) {
        match r.fields.iter_mut().find(|f| f.label == label) {
            Some(f) => f.value = Some(value),
            None => r.fields.push(RecordField {
                label: label.into(),
                value: Some(value),
            }),
        }
    }

    fn remove(r: &mut Record, label: &str) {
        r.fields.retain(|f| f.label != label);
    }

    fn chosen_mut(p: &mut PreparedTransaction) -> &mut Record {
        record_mut(exercise_mut(p).chosen_value.as_mut().unwrap())
    }

    fn input_mut<'a>(p: &'a mut PreparedTransaction, cid: &str) -> &'a mut Create {
        p.metadata
            .as_mut()
            .unwrap()
            .input_contracts
            .iter_mut()
            .find_map(|ic| match ic.contract.as_mut() {
                Some(InputContract::V1(c)) if c.contract_id == cid => Some(c),
                _ => None,
            })
            .unwrap()
    }

    fn args_mut(c: &mut Create) -> &mut Record {
        record_mut(c.argument.as_mut().unwrap())
    }

    fn attestation_mut(p: &mut PreparedTransaction) -> &mut Create {
        let node = p
            .transaction
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|n| n.node_id == "1");
        match node.unwrap().versioned_node.as_mut().unwrap() {
            proto_v2::interactive::daml_transaction::node::VersionedNode::V1(n) => {
                match n.node_type.as_mut().unwrap() {
                    NodeType::Create(c) => c,
                    _ => panic!("not a create"),
                }
            }
        }
    }

    fn mutated(f: impl FnOnce(&mut PreparedTransaction)) -> PreparedTransaction {
        let mut p = burn_tx();
        f(&mut p);
        p
    }

    // --- burn ---

    #[test]
    fn a_burn_with_the_expected_terms_is_accepted_and_reports_its_holdings() {
        let i = accepted(&burn_tx(), &burn_expectation());
        assert_eq!(i.burn_holding_cids, vec![H1.to_string(), H2.to_string()]);
        assert!(
            i.result.summary.contains("burns 10.5 USDCx"),
            "{}",
            i.result.summary
        );
    }

    #[test]
    fn a_burn_compares_amounts_as_exact_decimals() {
        let e = burn_expectation();
        rejected(
            &mutated(|p| set(chosen_mut(p), "amount", numeric("10.4999999999"))),
            &e,
            "burn amount",
        );
        rejected(
            &mutated(|p| {
                set(
                    args_mut(attestation_mut(p)),
                    "amount",
                    numeric("10.50000001"),
                )
            }),
            &e,
            "WithdrawAttestation amount",
        );
        // Another scale of the same amount is the same amount.
        accepted(
            &mutated(|p| set(chosen_mut(p), "amount", numeric("10.5"))),
            &e,
        );
    }

    #[test]
    fn a_burn_with_other_terms_is_rejected() {
        let e = burn_expectation();
        let other_to = "0x1234567890abcdef1234567890abcdef12345679";
        rejected(
            &mutated(|p| set(chosen_mut(p), "destinationRecipient", text(other_to))),
            &e,
            "destinationRecipient",
        );
        rejected(
            &mutated(|p| set(chosen_mut(p), "destinationDomain", int64(1))),
            &e,
            "destinationDomain",
        );
        rejected(
            &mutated(|p| set(chosen_mut(p), "requestId", text("other"))),
            &e,
            "requestId",
        );
        rejected(
            &mutated(|p| set(chosen_mut(p), "reference", text("x"))),
            &e,
            "reference",
        );
        rejected(
            &mutated(|p| set(chosen_mut(p), "holdingCids", list(vec![]))),
            &e,
            "no holdings",
        );
        rejected(
            &mutated(|p| {
                set(
                    chosen_mut(p),
                    "holdingCids",
                    list(vec![contract_id(H1), contract_id(H1)]),
                )
            }),
            &e,
            "listed twice",
        );
        // The expected recipient must itself be lowercase.
        let mut upper = burn_expectation();
        if let OperationExpectation::UsdcxBurn {
            destination_recipient,
            ..
        } = &mut upper
        {
            *destination_recipient = TO.to_uppercase().replace("0X", "0x");
        }
        rejected(&burn_tx(), &upper, "lowercase hex");
    }

    #[test]
    fn a_burn_must_act_as_the_party_alone_with_the_expected_command_id() {
        let e = burn_expectation();
        rejected(
            &mutated(|p| {
                let s = p
                    .metadata
                    .as_mut()
                    .unwrap()
                    .submitter_info
                    .as_mut()
                    .unwrap();
                s.act_as.push(OTHER.into());
            }),
            &e,
            "act_as",
        );
        rejected(
            &mutated(|p| exercise_mut(p).acting_parties.push(OTHER.into())),
            &e,
            "acting parties",
        );
        rejected(
            &mutated(|p| {
                p.metadata
                    .as_mut()
                    .unwrap()
                    .submitter_info
                    .as_mut()
                    .unwrap()
                    .command_id = usdcx::burn_command_id("another-request");
            }),
            &e,
            "command_id",
        );
        rejected(
            &mutated(|p| {
                p.metadata.as_mut().unwrap().synchronizer_id = usdcx::TESTNET.synchronizer_id.into()
            }),
            &e,
            "synchronizer",
        );
    }

    #[test]
    fn a_burn_on_a_look_alike_agreement_is_rejected() {
        let e = burn_expectation();
        // Same names, another package.
        rejected(
            &mutated(|p| {
                let mut id = bridge(T_AGREEMENT);
                id.package_id = "feed".into();
                exercise_mut(p).template_id = Some(id.clone());
                input_mut(p, AGREEMENT).template_id = Some(id);
            }),
            &e,
            "not an allowed utility-bridge-v0 version",
        );
        // An agreement DA did not sign.
        rejected(
            &mutated(|p| input_mut(p, AGREEMENT).signatories = vec![PARTY.into()]),
            &e,
            "is not signed by",
        );
        // Another party's agreement.
        rejected(
            &mutated(|p| set(args_mut(input_mut(p, AGREEMENT)), "user", party(OTHER))),
            &e,
            "user",
        );
        // Another bridge operator.
        rejected(
            &mutated(|p| {
                set(
                    args_mut(input_mut(p, AGREEMENT)),
                    "bridgeOperator",
                    party(OTHER),
                )
            }),
            &e,
            "bridgeOperator",
        );
        // Through an interface.
        rejected(
            &mutated(|p| exercise_mut(p).interface_id = Some(bridge(T_AGREEMENT))),
            &e,
            "interface",
        );
        // Another choice.
        rejected(
            &mutated(|p| exercise_mut(p).choice_id = CHOICE_MINT.into()),
            &e,
            "choice is",
        );
    }

    #[test]
    fn a_burn_spends_only_the_partys_unlocked_usdcx() {
        let e = burn_expectation();
        rejected(
            &mutated(|p| set(args_mut(input_mut(p, H2)), "owner", party(OTHER))),
            &e,
            "owner",
        );
        rejected(
            &mutated(|p| {
                let lock = record(vec![("lockers", list(vec![party(OTHER)]))]);
                set(args_mut(input_mut(p, H2)), "lock", optional(Some(lock)));
            }),
            &e,
            "lock",
        );
        rejected(
            &mutated(|p| {
                let instrument = args_mut(input_mut(p, H1))
                    .fields
                    .iter_mut()
                    .find(|f| f.label == "instrument")
                    .unwrap();
                set(
                    record_mut(instrument.value.as_mut().unwrap()),
                    "id",
                    text("USDC"),
                );
            }),
            &e,
            "id",
        );
        rejected(
            &mutated(|p| input_mut(p, H1).signatories = vec![PARTY.into()]),
            &e,
            "not signed by",
        );
        // A holding missing from the inputs.
        rejected(
            &mutated(|p| {
                set(
                    chosen_mut(p),
                    "holdingCids",
                    list(vec![contract_id(H1), contract_id("00b3")]),
                )
            }),
            &e,
            "not among the input contracts",
        );
        // An absent lock (a trailing None left out) is unlocked.
        accepted(&mutated(|p| remove(args_mut(input_mut(p, H1)), "lock")), &e);
    }

    #[test]
    fn a_burn_needs_das_factory_and_no_app_reward_routing() {
        let e = burn_expectation();
        rejected(
            &mutated(|p| set(args_mut(input_mut(p, FACTORY)), "registrar", party(OTHER))),
            &e,
            "registrar",
        );
        rejected(
            &mutated(|p| input_mut(p, FACTORY).signatories = vec![OTHER.into()]),
            &e,
            "not signed by",
        );
        rejected(
            &mutated(|p| {
                let ctx = chosen_mut(p)
                    .fields
                    .iter_mut()
                    .find(|f| f.label == "contextContractIds")
                    .unwrap();
                set(
                    record_mut(ctx.value.as_mut().unwrap()),
                    "featuredAppRightCid",
                    optional(Some(contract_id("00aa"))),
                );
            }),
            &e,
            "featuredAppRightCid",
        );
        rejected(
            &mutated(|p| input_mut(p, CONFIG).signatories = vec![OTHER.into()]),
            &e,
            "not signed by",
        );
    }

    #[test]
    fn a_burn_creates_exactly_one_matching_withdraw_attestation() {
        let e = burn_expectation();
        rejected(
            &mutated(|p| {
                let extra = create_node("2", withdraw_attestation());
                p.transaction.as_mut().unwrap().nodes.push(extra);
                exercise_mut(p).children.push("2".into());
            }),
            &e,
            "exactly one WithdrawAttestation",
        );
        rejected(
            &mutated(|p| {
                p.transaction
                    .as_mut()
                    .unwrap()
                    .nodes
                    .retain(|n| n.node_id != "1");
                exercise_mut(p).children.clear();
            }),
            &e,
            "exactly one WithdrawAttestation",
        );
        rejected(
            &mutated(|p| {
                set(
                    args_mut(attestation_mut(p)),
                    "sourceDepositor",
                    valid(OTHER),
                )
            }),
            &e,
            "sourceDepositor",
        );
        rejected(
            &mutated(|p| set(args_mut(attestation_mut(p)), "requestId", text("x"))),
            &e,
            "requestId",
        );
        rejected(
            &mutated(|p| attestation_mut(p).signatories = vec![NET.admin.into(), PARTY.into()]),
            &e,
            "not signed by",
        );
    }

    #[test]
    fn a_second_root_is_rejected() {
        let p = mutated(|p| {
            let tx = p.transaction.as_mut().unwrap();
            tx.roots.push("1".into());
        });
        rejected(&p, &burn_expectation(), "exactly one root");
    }

    #[test]
    fn a_hash_mismatch_or_failure_rejects_a_usdcx_operation() {
        let p = burn_tx();
        let bytes = encode(&p);
        let computed = crate::hasher::compute_hash(&bytes, "V2").unwrap();
        assert_ne!(computed, [0u8; 32]);

        let ok = crate::verify_and_hash(
            &bytes,
            &BASE64.encode(computed),
            "V2",
            &burn_expectation(),
            false,
        )
        .unwrap();
        assert!(ok.accepted, "{:?}", ok.rejection_reason);
        assert_eq!(ok.computed_hash, computed);
        assert_eq!(ok.burn_holding_cids, vec![H1.to_string(), H2.to_string()]);

        let mismatch = crate::verify_and_hash(
            &bytes,
            &BASE64.encode([7u8; 32]),
            "V2",
            &burn_expectation(),
            false,
        )
        .unwrap();
        assert!(!mismatch.accepted);
        assert!(mismatch.rejection_reason.unwrap().contains("Hash mismatch"));
        assert!(mismatch.burn_holding_cids.is_empty());

        // An exercise without a seed cannot be hashed.
        let mut unseeded = burn_tx();
        unseeded.transaction.as_mut().unwrap().node_seeds.clear();
        let unseeded = encode(&unseeded);
        assert_eq!(
            crate::hasher::compute_hash(&unseeded, "V2").unwrap(),
            [0u8; 32]
        );
        let failed = crate::verify_and_hash(
            &unseeded,
            &BASE64.encode(computed),
            "V2",
            &burn_expectation(),
            false,
        )
        .unwrap();
        assert!(!failed.accepted);
        assert!(
            failed
                .rejection_reason
                .unwrap()
                .contains("could not be computed")
        );
    }

    #[test]
    fn the_generic_inspector_never_stubs_a_usdcx_operation() {
        let p = mutated(|p| set(chosen_mut(p), "amount", numeric("1")));
        let r = crate::inspector::inspect(&encode(&p), &burn_expectation(), false).unwrap();
        assert!(!r.accepted);
        assert!(!r.summary.contains("[STUB]"));
    }

    // --- mint, reclaim ---

    fn deposit_attestation(recipient: &str) -> Create {
        let mut f = da_fields();
        f.extend([
            ("recipient", party(recipient)),
            ("amount", numeric("10.0000000000")),
            (
                "sourceDepositor",
                text("000000000000000000000000aabbccddeeff00112233445566778899aabbccdd"),
            ),
            ("reference", text("")),
        ]);
        with_cid(
            create(
                bridge(T_DEPOSIT_ATTESTATION),
                record(f),
                &[NET.admin, NET.bridge_operator],
            ),
            DEPOSIT,
        )
    }

    fn mint_tx(attestation: Create) -> PreparedTransaction {
        let chosen = record(vec![
            ("depositAttestationCid", contract_id(DEPOSIT)),
            ("factoryCid", contract_id(FACTORY)),
            ("contextContractIds", context()),
        ]);
        let root = exercise_node("0", agreement_exercise(CHOICE_MINT, chosen, vec![]));
        finish(
            prepared(&["0"], vec![root], vec![seed(0, 1)], &[PARTY]),
            &usdcx::mint_command_id(DEPOSIT),
            vec![agreement(), attestation, factory(), config()],
        )
    }

    fn mint_expectation() -> OperationExpectation {
        OperationExpectation::UsdcxMint {
            party: PARTY.into(),
            network: "mainnet".into(),
            deposit_attestation_cid: DEPOSIT.into(),
            command_id: usdcx::mint_command_id(DEPOSIT),
        }
    }

    #[test]
    fn a_mint_of_the_partys_attestation_is_accepted() {
        let i = accepted(&mint_tx(deposit_attestation(PARTY)), &mint_expectation());
        assert!(i.burn_holding_cids.is_empty());
        rejected(
            &mint_tx(deposit_attestation(OTHER)),
            &mint_expectation(),
            "recipient",
        );
        let mut unsigned = deposit_attestation(PARTY);
        unsigned.signatories = vec![NET.admin.into()];
        rejected(&mint_tx(unsigned), &mint_expectation(), "not signed by");
        let mut other = mint_expectation();
        if let OperationExpectation::UsdcxMint {
            deposit_attestation_cid,
            ..
        } = &mut other
        {
            *deposit_attestation_cid = "00d2".into();
        }
        rejected(
            &mint_tx(deposit_attestation(PARTY)),
            &other,
            "depositAttestationCid",
        );
        // A mint of another attestation also carries another command id.
        if let OperationExpectation::UsdcxMint { command_id, .. } = &mut other {
            *command_id = usdcx::mint_command_id("00d2");
        }
        rejected(&mint_tx(deposit_attestation(PARTY)), &other, "command_id");
    }

    fn reclaim_tx(depositor: &str) -> PreparedTransaction {
        let mut att = da_fields();
        att.extend([
            ("sourceDepositor", valid(depositor)),
            ("requestId", text(REQUEST)),
            ("amount", numeric("10.0000000000")),
            ("destinationDomain", int64(0)),
            ("destinationRecipient", text(TO)),
        ]);
        let revoked = with_cid(
            create(
                bridge(T_REVOKED_WITHDRAW),
                record(vec![
                    ("attestation", record(att)),
                    ("reason", text("blocked")),
                ]),
                &[NET.admin, NET.bridge_operator],
            ),
            REVOKED,
        );
        let chosen = record(vec![
            ("revokedWithdrawAttestationCid", contract_id(REVOKED)),
            ("factoryCid", contract_id(FACTORY)),
            ("contextContractIds", context()),
        ]);
        let root = exercise_node("0", agreement_exercise(CHOICE_RECLAIM, chosen, vec![]));
        finish(
            prepared(&["0"], vec![root], vec![seed(0, 1)], &[PARTY]),
            &usdcx::reclaim_command_id(REVOKED),
            vec![agreement(), revoked, factory(), config()],
        )
    }

    #[test]
    fn a_reclaim_of_the_partys_revoked_withdrawal_is_accepted() {
        let e = OperationExpectation::UsdcxReclaimHolding {
            party: PARTY.into(),
            network: "mainnet".into(),
            revoked_withdraw_attestation_cid: REVOKED.into(),
            command_id: usdcx::reclaim_command_id(REVOKED),
        };
        accepted(&reclaim_tx(PARTY), &e);
        rejected(&reclaim_tx(OTHER), &e, "sourceDepositor");
    }

    // --- onboarding, pre-approval, offboarding ---

    fn request_tx(f: impl FnOnce(&mut Vec<(&'static str, Value)>)) -> PreparedTransaction {
        let mut fields = da_fields();
        fields.push(("user", party(PARTY)));
        fields.push(("preApproval", boolean(true)));
        f(&mut fields);
        let root = create_node(
            "0",
            create(bridge(T_AGREEMENT_REQUEST), record(fields), &[PARTY]),
        );
        finish(
            prepared(&["0"], vec![root], vec![], &[PARTY]),
            &usdcx::onboard_command_id(PARTY, REQUEST),
            vec![],
        )
    }

    fn request_expectation() -> OperationExpectation {
        OperationExpectation::UsdcxRequestAgreement {
            party: PARTY.into(),
            network: "mainnet".into(),
            pre_approval: true,
            command_id: usdcx::onboard_command_id(PARTY, REQUEST),
        }
    }

    #[test]
    fn an_onboarding_request_to_das_parties_is_accepted() {
        let e = request_expectation();
        accepted(&request_tx(|_| {}), &e);
        rejected(
            &request_tx(|f| f.retain(|(l, _)| *l != "bridgeOperator")),
            &e,
            "bridgeOperator",
        );
        rejected(
            &request_tx(|f| {
                f.retain(|(l, _)| *l != "bridgeOperator");
                f.push(("bridgeOperator", party(OTHER)));
            }),
            &e,
            "bridgeOperator",
        );
        rejected(
            &request_tx(|f| {
                f.retain(|(l, _)| *l != "preApproval");
                f.push(("preApproval", boolean(false)));
            }),
            &e,
            "preApproval",
        );
        // On TestNet the same request names other parties.
        let mut testnet = request_expectation();
        if let OperationExpectation::UsdcxRequestAgreement { network, .. } = &mut testnet {
            *network = "testnet".into();
        }
        rejected(&request_tx(|_| {}), &testnet, "synchronizer");
        let mut devnet = request_expectation();
        if let OperationExpectation::UsdcxRequestAgreement { network, .. } = &mut devnet {
            *network = "devnet".into();
        }
        rejected(&request_tx(|_| {}), &devnet, "not available");
    }

    #[test]
    fn pre_approval_and_offboarding_need_the_requested_arguments() {
        let update = |value: bool| {
            let root = exercise_node(
                "0",
                agreement_exercise(
                    CHOICE_UPDATE_PRE_APPROVAL,
                    record(vec![("preApproval", boolean(value))]),
                    vec![],
                ),
            );
            finish(
                prepared(&["0"], vec![root], vec![seed(0, 1)], &[PARTY]),
                "usdcx-preapproval-x",
                vec![agreement()],
            )
        };
        let e = OperationExpectation::UsdcxUpdatePreApproval {
            party: PARTY.into(),
            network: "mainnet".into(),
            pre_approval: true,
        };
        accepted(&update(true), &e);
        rejected(&update(false), &e, "preApproval");

        let terminate = |actor: &str| {
            let chosen = record(vec![("reason", text("done")), ("actor", party(actor))]);
            let root = exercise_node("0", agreement_exercise(CHOICE_TERMINATE, chosen, vec![]));
            finish(
                prepared(&["0"], vec![root], vec![seed(0, 1)], &[PARTY]),
                "usdcx-offboard-x",
                vec![agreement()],
            )
        };
        let e = OperationExpectation::UsdcxTerminateAgreement {
            party: PARTY.into(),
            network: "mainnet".into(),
            reason: "done".into(),
        };
        accepted(&terminate(PARTY), &e);
        rejected(&terminate(OTHER), &e, "actor");
    }
}
