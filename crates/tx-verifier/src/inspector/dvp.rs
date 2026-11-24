//! DvP inspection: propose, accept, allocate and proposal cleanup.

use super::*;
use crate::decimal::parse_exact;
use crate::facts::{extract_facts, AllocationFact, DvpTerms, FlowKind, Quantity};
use crate::types::{ExpectedDvpTerms, ExpectedQuantity};

/// The warning of a DvP step that carries no terms recorded by the agent.
const NO_TERMS: &str = "no recorded DvP terms: the transaction's terms are not compared";

/// The module of the allocation factory interface.
const ALLOCATION_FACTORY_MODULE: &str = "Splice.Api.Token.AllocationInstructionV1";

/// ProposeDvp: the agent's own proposal.
pub(super) fn inspect_propose_dvp(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::ProposeDvp { proposer, counterparty, proposal_id, synchronizer_id, terms } = expectation
    else {
        return Err(wrong_expectation("ProposeDvp"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    let mut result = check_propose_dvp(&prepared, proposer, proposal_id, synchronizer_id)?;
    if result.accepted {
        match terms {
            None => result.warnings.push(NO_TERMS.to_string()),
            Some(terms) => {
                observed.extend(terms_findings("ProposeDvp", prepared_transaction_bytes, proposer, proposal_id, terms));
                observed.extend(proposer_findings(&prepared, proposer, counterparty, terms));
            }
        }
    }
    Ok(result)
}

/// AcceptDvp: the agent accepts a proposal.
pub(super) fn inspect_accept_dvp(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::AcceptDvp {
        acceptor,
        proposal_id,
        dvp_proposal_cid,
        expected_delivery_amount,
        expected_payment_amount,
        expected_delivery_instrument_id,
        expected_delivery_instrument_admin,
        expected_payment_instrument_id,
        expected_payment_instrument_admin,
        terms,
    } = expectation
    else {
        return Err(wrong_expectation("AcceptDvp"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    let delivery = ExpectedLeg {
        amount: expected_delivery_amount,
        instrument_id: expected_delivery_instrument_id,
        instrument_admin: expected_delivery_instrument_admin,
    };
    let payment = ExpectedLeg {
        amount: expected_payment_amount,
        instrument_id: expected_payment_instrument_id,
        instrument_admin: expected_payment_instrument_admin,
    };
    let mut result = check_accept_dvp(
        &prepared,
        acceptor,
        proposal_id,
        dvp_proposal_cid,
        &delivery,
        &payment,
    )?;
    if result.accepted {
        match terms {
            None => result.warnings.push(NO_TERMS.to_string()),
            Some(terms) => {
                observed.extend(terms_findings("AcceptDvp", prepared_transaction_bytes, acceptor, proposal_id, terms));
            }
        }
    }
    Ok(result)
}

/// Allocate: the agent funds its leg of a DvP.
pub(super) fn inspect_allocate(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::Allocate { party, proposal_id, dvp_cid, terms, settlement_operator } = expectation else {
        return Err(wrong_expectation("Allocate"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    let mut result = check_allocate(&prepared, party, proposal_id, dvp_cid)?;
    if result.accepted {
        if terms.is_none() {
            result.warnings.push(NO_TERMS.to_string());
        }
        observed.extend(allocation_findings(
            &prepared,
            prepared_transaction_bytes,
            party,
            proposal_id,
            terms.as_ref(),
            settlement_operator.as_deref(),
        ));
    }
    Ok(result)
}

/// ExecuteMulticall: its operations are not checked yet, but the DvP terms it carries
/// and the agent's allocations for them are compared with the terms the agent recorded.
pub(super) fn inspect_execute_multicall(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::ExecuteMulticall { party, terms, .. } = expectation else {
        return Err(wrong_expectation("ExecuteMulticall"));
    };
    let result = not_yet_verified(prepared_transaction_bytes, expectation);
    let Some(terms) = terms else {
        return Ok(result);
    };
    let op = "ExecuteMulticall";
    observed.extend(terms_findings(op, prepared_transaction_bytes, party, &terms.proposal_id, terms));
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    if let Ok(facts) = extract_facts(prepared_transaction_bytes, party) {
        for allocation in facts.allocations.iter().filter(|a| a.sender == *party) {
            if allocation.settlement_ref_id != terms.proposal_id {
                observed.push(format!(
                    "{op}: the allocation's settlementRef id {} is not the proposal {}",
                    allocation.settlement_ref_id, terms.proposal_id
                ));
            }
            observed.extend(deadline_findings(op, &prepared, allocation));
        }
    }
    Ok(result)
}

// ============================================================================
// Recorded DvP terms (Fields checks that observe first)
// ============================================================================

fn quantity_agrees(q: &Quantity, e: &ExpectedQuantity) -> bool {
    q.instrument_admin == e.instrument_admin
        && q.instrument_id == e.instrument_id
        && parse_exact(&e.amount) == Some(q.amount)
}

fn terms_agree(t: &DvpTerms, e: &ExpectedDvpTerms) -> bool {
    t.proposal_id.as_deref() == Some(e.proposal_id.as_str())
        && t.buyer == e.buyer
        && t.seller == e.seller
        && quantity_agrees(&t.delivery, &e.delivery)
        && quantity_agrees(&t.payment, &e.payment)
}

fn describe_terms(id: &str, buyer: &str, seller: &str, delivery: String, payment: String) -> String {
    format!(
        "{id} (buyer {}, seller {}, {delivery} for {payment})",
        short(buyer, 16),
        short(seller, 16)
    )
}

/// The DvP terms the transaction carries, against the terms the agent recorded.
fn terms_findings(
    op: &str,
    prepared_transaction_bytes: &[u8],
    party: &str,
    proposal_id: &str,
    terms: &ExpectedDvpTerms,
) -> Vec<String> {
    let mut found = Vec::new();
    if terms.proposal_id != proposal_id {
        found.push(format!(
            "{op}: the recorded terms are of proposal {}, not {proposal_id}",
            terms.proposal_id
        ));
    }
    if terms.buyer != party && terms.seller != party {
        found.push(format!("{op}: the agent is neither buyer nor seller of the recorded terms"));
    }
    let facts = match extract_facts(prepared_transaction_bytes, party) {
        Ok(facts) => facts,
        Err(e) => {
            found.push(format!("{op}: the transaction's DvP terms are unreadable: {e}"));
            return found;
        }
    };
    if facts.dvp.is_empty() {
        found.push(format!("{op}: the transaction carries no DvP terms"));
    }
    let expected = describe_terms(
        &terms.proposal_id,
        &terms.buyer,
        &terms.seller,
        format!("{} {}", terms.delivery.amount, terms.delivery.instrument_id),
        format!("{} {}", terms.payment.amount, terms.payment.instrument_id),
    );
    for t in facts.dvp.iter().filter(|t| !terms_agree(t, terms)) {
        let got = describe_terms(
            t.proposal_id.as_deref().unwrap_or("(no id)"),
            &t.buyer,
            &t.seller,
            format!("{} {}", t.delivery.amount, t.delivery.instrument_id),
            format!("{} {}", t.payment.amount, t.payment.instrument_id),
        );
        found.push(format!("{op}: the transaction's DvP terms {got} are not the recorded terms {expected}"));
    }
    found
}

/// The created proposal names the agent as proposer and the other party of the terms.
fn proposer_findings(
    prepared: &PreparedTransaction,
    proposer: &str,
    counterparty: &str,
    terms: &ExpectedDvpTerms,
) -> Vec<String> {
    let mut found = Vec::new();
    let other = if terms.buyer == proposer { terms.seller.as_str() } else { terms.buyer.as_str() };
    if !counterparty.is_empty() && counterparty != other {
        found.push(format!(
            "ProposeDvp: the counterparty {} is not the other party {} of the recorded terms",
            short(counterparty, 16),
            short(other, 16)
        ));
    }
    let foreign = prepared
        .transaction
        .iter()
        .flat_map(|tx| &tx.nodes)
        .filter_map(|node| match get_node_type(node) {
            Some(NodeType::Create(c))
                if c.template_id
                    .as_ref()
                    .is_some_and(|t| template_matches(t, DVP_MODULE, "DvpProposal")) =>
            {
                Some(c)
            }
            _ => None,
        })
        .any(|c| {
            let named = c
                .argument
                .as_ref()
                .and_then(as_record)
                .and_then(|r| get_record_field(r, "proposer"))
                .and_then(get_party);
            named != Some(proposer)
        });
    if foreign {
        found.push("ProposeDvp: the created proposal's proposer is not the agent".to_string());
    }
    found
}

/// The receiver and quantity of the agent's own leg: the seller delivers, the buyer pays.
fn recorded_leg<'a>(party: &str, terms: &'a ExpectedDvpTerms) -> Option<(&'a str, &'a ExpectedQuantity)> {
    if terms.seller == party {
        Some((terms.buyer.as_str(), &terms.delivery))
    } else if terms.buyer == party {
        Some((terms.seller.as_str(), &terms.payment))
    } else {
        None
    }
}

/// The longest a settlement may stay open after its request or this transaction: 72 h.
const MAX_SETTLE_WINDOW_MICROS: i64 = 259_200_000_000;

/// `allocation.settlement.requestedAt` of the allocation contract `cid` the transaction creates.
fn requested_at_micros(prepared: &PreparedTransaction, cid: &str) -> Option<i64> {
    let created = prepared.transaction.iter().flat_map(|tx| &tx.nodes).find_map(|node| match get_node_type(node) {
        Some(NodeType::Create(c)) if c.contract_id == cid => c.argument.as_ref(),
        _ => None,
    })?;
    let settlement = as_record(created)
        .and_then(|r| get_record_field(r, "allocation"))
        .and_then(as_record)
        .and_then(|r| get_record_field(r, "settlement"))
        .and_then(as_record)?;
    match get_record_field(settlement, "requestedAt").and_then(|v| v.sum.as_ref()) {
        Some(Sum::Timestamp(t)) => Some(*t),
        _ => None,
    }
}

/// The allocation's deadlines: it is allocated before it settles, and it settles within
/// [`MAX_SETTLE_WINDOW_MICROS`] of its request and of this transaction's preparation.
fn deadline_findings(op: &str, prepared: &PreparedTransaction, allocation: &AllocationFact) -> Vec<String> {
    let mut found = Vec::new();
    let settle_before = allocation.settle_before_micros;
    if allocation.allocate_before_micros > settle_before {
        found.push(format!("{op}: the allocation deadline is after the settlement deadline"));
    }
    let prepared_at = prepared
        .metadata
        .as_ref()
        .and_then(|m| i64::try_from(m.preparation_time).ok())
        .filter(|t| *t > 0);
    let starts = [("request", requested_at_micros(prepared, &allocation.contract_id)), ("preparation", prepared_at)];
    for (what, start) in starts {
        match start.map(|t| settle_before.checked_sub(t)) {
            None if what == "request" => found.push(format!("{op}: the allocation's requestedAt is unreadable")),
            None => {}
            Some(window) if window.is_none_or(|w| w > MAX_SETTLE_WINDOW_MICROS) => {
                found.push(format!("{op}: the settlement deadline is more than 72 h after the {what}"));
            }
            Some(_) => {}
        }
    }
    found
}

/// The agent alone allocates the one leg its recorded terms give it, for this
/// proposal and executor, and every other outflow stays with it.
fn allocation_findings(
    prepared: &PreparedTransaction,
    prepared_transaction_bytes: &[u8],
    party: &str,
    proposal_id: &str,
    terms: Option<&ExpectedDvpTerms>,
    settlement_operator: Option<&str>,
) -> Vec<String> {
    let mut found = Vec::new();
    let parts = match extract_root_exercise(prepared, "Allocate") {
        Ok(parts) => parts,
        Err(rejection) => {
            found.push(format!("Allocate: {}", rejection.rejection_reason.unwrap_or_default()));
            return found;
        }
    };
    let root = parts.root_exercise;
    if parts.tx.roots.len() != 1 {
        found.push(format!("Allocate: expected 1 root node, got {}", parts.tx.roots.len()));
    }
    let on_factory = root
        .interface_id
        .as_ref()
        .is_some_and(|i| template_matches(i, ALLOCATION_FACTORY_MODULE, "AllocationFactory"));
    if !on_factory {
        found.push("Allocate: the root is not exercised on the AllocationFactory interface".to_string());
    }
    if parts.submitter.act_as != [party] || root.acting_parties != [party] {
        found.push(format!(
            "Allocate: act_as {:?} and acting parties {:?} are not exactly the agent",
            parts.submitter.act_as, root.acting_parties
        ));
    }
    let facts = match extract_facts(prepared_transaction_bytes, party) {
        Ok(facts) => facts,
        Err(e) => {
            found.push(format!("Allocate: the transaction facts are unreadable: {e}"));
            return found;
        }
    };
    let [allocation] = facts.allocations.as_slice() else {
        found.push(format!(
            "Allocate: expected exactly one allocation, got {}",
            facts.allocations.len()
        ));
        return found;
    };
    if allocation.settlement_ref_id != proposal_id {
        found.push(format!(
            "Allocate: the allocation's settlementRef id {} is not the proposal {proposal_id}",
            allocation.settlement_ref_id
        ));
    }
    if allocation.sender != party {
        found.push(format!(
            "Allocate: the allocated leg's sender {} is not the agent",
            short(&allocation.sender, 16)
        ));
    }
    if let Some(operator) = settlement_operator.filter(|o| allocation.executor != *o) {
        found.push(format!(
            "Allocate: the settlement executor {} is not the pinned settlement operator {}",
            short(&allocation.executor, 16),
            short(operator, 16)
        ));
    }
    found.extend(deadline_findings("Allocate", prepared, allocation));
    if let Some(terms) = terms {
        match recorded_leg(party, terms) {
            None => found.push("Allocate: the agent is neither buyer nor seller of the recorded terms".to_string()),
            Some((receiver, q)) => {
                let same = terms.proposal_id == proposal_id
                    && allocation.receiver == receiver
                    && allocation.instrument_admin == q.instrument_admin
                    && allocation.instrument_id == q.instrument_id
                    && parse_exact(&q.amount) == Some(allocation.amount);
                if !same {
                    found.push(format!(
                        "Allocate: the allocated leg {} {} to {} is not the recorded leg {} {} to {} of proposal {}",
                        allocation.amount,
                        allocation.instrument_id,
                        short(&allocation.receiver, 16),
                        q.amount,
                        q.instrument_id,
                        short(receiver, 16),
                        terms.proposal_id
                    ));
                }
            }
        }
    }
    let expected_admin = root
        .chosen_value
        .as_ref()
        .and_then(as_record)
        .and_then(|r| get_record_field(r, "expectedAdmin"))
        .and_then(get_party);
    if expected_admin != Some(allocation.instrument_admin.as_str()) {
        found.push("Allocate: expectedAdmin is not the allocated leg's instrument admin".to_string());
    }
    let mut legs = 0usize;
    for flow in facts.outflows.iter().filter(|f| f.to != party) {
        let is_leg = flow.kind == FlowKind::Allocation
            && flow.to == allocation.receiver
            && flow.instrument_admin == allocation.instrument_admin
            && flow.instrument_id == allocation.instrument_id
            && flow.amount == allocation.amount;
        legs = legs.saturating_add(usize::from(is_leg));
        if !is_leg || legs > 1 {
            found.push(format!(
                "Allocate: an outflow of {} {} ({:?}) to {} is not the one allocated leg",
                flow.amount,
                flow.instrument_id,
                flow.kind,
                short(&flow.to, 16)
            ));
        }
    }
    found
}

/// CancelDvpProposal and RejectDvpProposal.
pub(super) fn inspect_dvp_proposal_gc(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    _observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let (party, dvp_proposal_cid, choice) = match expectation {
        OperationExpectation::CancelDvpProposal { party, dvp_proposal_cid } => {
            (party, dvp_proposal_cid, "DvpProposal_Cancel")
        }
        OperationExpectation::RejectDvpProposal { party, dvp_proposal_cid } => {
            (party, dvp_proposal_cid, "DvpProposal_Reject")
        }
        _ => return Err(wrong_expectation("DvpProposal cleanup")),
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    check_dvp_proposal_gc(&prepared, party, dvp_proposal_cid, choice)
}

// ============================================================================
// ProposeDvp inspection (Phase B)
// ============================================================================

fn check_propose_dvp(
    prepared: &PreparedTransaction,
    buyer_party: &str,
    proposal_id: &str,
    synchronizer_id: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, "ProposeDvp") {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Metadata: act_as ---
    if !parts.submitter.act_as.contains(&buyer_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "ProposeDvp: buyer not in act_as".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain buyer_party {}",
                parts.submitter.act_as, buyer_party
            )),
        });
    }

    // --- Metadata: synchronizer_id, as logical ids (the transaction may name a physical id) ---
    if !synchronizer_id.is_empty()
        && logical_synchronizer_id(&parts.metadata.synchronizer_id)
            != logical_synchronizer_id(synchronizer_id)
    {
        warnings.push(format!(
            "synchronizer_id mismatch: expected={}, got={}",
            synchronizer_id, parts.metadata.synchronizer_id
        ));
    }

    // --- Metadata: command_id ---
    let expected_cmd = format!("dvp-propose-{}", proposal_id);
    if parts.submitter.command_id != expected_cmd {
        warnings.push(format!(
            "command_id mismatch: expected={}, got={}",
            expected_cmd, parts.submitter.command_id
        ));
    }

    // --- Root exercise: choice_id ---
    if parts.root_exercise.choice_id != "UserService_ProposeDvp" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("ProposeDvp: wrong choice_id: {}", parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=UserService_ProposeDvp, got={}",
                parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: UserService ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "Utility.Settlement.App.V1.Service.User", "UserService") {
            return Ok(InspectionResult {
                accepted: false,
                summary: format!("ProposeDvp: wrong template: {}.{}", tid.module_name, tid.entity_name),
                warnings,
                rejection_reason: Some(format!(
                    "Expected UserService template, got {}.{}",
                    tid.module_name, tid.entity_name
                )),
            });
        }
    }

    // --- acting_parties ---
    if !parts.root_exercise.acting_parties.contains(&buyer_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "ProposeDvp: buyer not in acting_parties".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain buyer {}",
                parts.root_exercise.acting_parties, buyer_party
            )),
        });
    }

    // --- chosen_value: terms.id must match proposal_id ---
    if let Some(chosen) = parts.root_exercise.chosen_value.as_ref() {
        if let Some(cr) = as_record(chosen) {
            let terms_id = get_record_field(cr, "terms")
                .and_then(as_record)
                .and_then(|t| get_record_field(t, "id"))
                .and_then(get_text);
            if let Some(tid) = terms_id {
                if tid != proposal_id {
                    return Ok(InspectionResult {
                        accepted: false,
                        summary: "ProposeDvp: wrong terms.id".to_string(),
                        warnings,
                        rejection_reason: Some(format!(
                            "chosen_value.terms.id={}, expected={}",
                            tid, proposal_id
                        )),
                    });
                }
            } else {
                warnings.push("Could not extract chosen_value.terms.id".to_string());
            }
        }
    }

    // --- Child Create node: DvpProposal template ---
    let mut dvp_proposal_created = false;
    for child_id in &parts.root_exercise.children {
        if let Some(child_node) = parts.nodes_dict.get(child_id) {
            if let Some(NodeType::Create(create)) = get_node_type(child_node) {
                if let Some(tid) = &create.template_id {
                    if template_matches(tid, "Utility.Settlement.App.V1.Model.Dvp", "DvpProposal") {
                        dvp_proposal_created = true;
                        // Verify created DvpProposal terms.id
                        if let Some(arg) = &create.argument {
                            if let Some(rec) = as_record(arg) {
                                let created_id = get_record_field(rec, "terms")
                                    .and_then(as_record)
                                    .and_then(|t| get_record_field(t, "id"))
                                    .and_then(get_text);
                                if let Some(cid) = created_id {
                                    if cid != proposal_id {
                                        return Ok(InspectionResult {
                                            accepted: false,
                                            summary: "ProposeDvp: created DvpProposal has wrong terms.id".to_string(),
                                            warnings,
                                            rejection_reason: Some(format!(
                                                "DvpProposal.terms.id={}, expected={}",
                                                cid, proposal_id
                                            )),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !dvp_proposal_created {
        warnings.push("No DvpProposal Create node found in children".to_string());
    }

    let summary = format!(
        "ProposeDvp VERIFIED: buyer {} proposes {} | {}",
        short(buyer_party, 8),
        proposal_id,
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// DvpProposal GC inspection (Cancel / Reject)
// ============================================================================

/// Verify a direct DvpProposal_Cancel / DvpProposal_Reject exercise: exactly
/// one root, the expected choice on the expected DvpProposal cid, our party
/// acting. Cancel must create nothing; Reject may create only the
/// RejectedDvpProposal.
fn check_dvp_proposal_gc(
    prepared: &PreparedTransaction,
    party: &str,
    dvp_proposal_cid: &str,
    choice: &str, // "DvpProposal_Cancel" | "DvpProposal_Reject"
) -> Result<InspectionResult> {
    let op_name = choice;
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, op_name) {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Single root: nothing may ride along with the archive ---
    if parts.tx.roots.len() != 1 {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: expected 1 root, got {}", op_name, parts.tx.roots.len()),
            warnings,
            rejection_reason: Some(format!(
                "Transaction has {} root nodes, expected exactly 1",
                parts.tx.roots.len()
            )),
        });
    }

    // --- Metadata: act_as ---
    if !parts.submitter.act_as.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: party not in act_as", op_name),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain party {}",
                parts.submitter.act_as, party
            )),
        });
    }

    // --- Metadata: command_id ---
    let expected_cmd = format!("dvp-proposal-gc-{}", dvp_proposal_cid);
    if parts.submitter.command_id != expected_cmd {
        warnings.push(format!(
            "command_id mismatch: expected={}, got={}",
            expected_cmd, parts.submitter.command_id
        ));
    }

    // --- Root exercise: choice_id ---
    if parts.root_exercise.choice_id != choice {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: wrong choice_id: {}", op_name, parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id={}, got={}",
                choice, parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: DvpProposal ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "Utility.Settlement.App.V1.Model.Dvp", "DvpProposal") {
            return Ok(InspectionResult {
                accepted: false,
                summary: format!("{}: wrong template: {}.{}", op_name, tid.module_name, tid.entity_name),
                warnings,
                rejection_reason: Some(format!(
                    "Expected DvpProposal template, got {}.{}",
                    tid.module_name, tid.entity_name
                )),
            });
        }
    }

    // --- contract_id must match the expected DvpProposal ---
    if parts.root_exercise.contract_id != dvp_proposal_cid {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: wrong contract_id", op_name),
            warnings,
            rejection_reason: Some(format!(
                "contract_id={}, expected={}",
                parts.root_exercise.contract_id, dvp_proposal_cid
            )),
        });
    }

    // --- acting_parties ---
    if !parts.root_exercise.acting_parties.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: party not in acting_parties", op_name),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain party {}",
                parts.root_exercise.acting_parties, party
            )),
        });
    }

    // --- Children: Cancel creates nothing; Reject only a RejectedDvpProposal ---
    for child_id in &parts.root_exercise.children {
        if let Some(child_node) = parts.nodes_dict.get(child_id) {
            if let Some(NodeType::Create(create)) = get_node_type(child_node) {
                let allowed = choice == "DvpProposal_Reject"
                    && create.template_id.as_ref().is_some_and(|tid| {
                        template_matches(tid, "Utility.Settlement.App.V1.Model.Dvp", "RejectedDvpProposal")
                    });
                if !allowed {
                    let tname = create.template_id.as_ref()
                        .map(|t| format!("{}.{}", t.module_name, t.entity_name))
                        .unwrap_or_else(|| "unknown".to_string());
                    return Ok(InspectionResult {
                        accepted: false,
                        summary: format!("{}: unexpected Create of {}", op_name, tname),
                        warnings,
                        rejection_reason: Some(format!(
                            "{} must not create {} — only DvpProposal_Reject may create RejectedDvpProposal",
                            op_name, tname
                        )),
                    });
                }
            }
        }
    }

    let summary = format!(
        "{} VERIFIED: party {} archives {} | {}",
        op_name,
        short(party, 8),
        short(dvp_proposal_cid, 16),
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// AcceptDvp inspection (Phase B)
// ============================================================================

const DVP_MODULE: &str = "Utility.Settlement.App.V1.Model.Dvp";

/// The terms one leg of the accepted DvpProposal must carry.
struct ExpectedLeg<'a> {
    amount: &'a str,
    instrument_id: &'a str,
    instrument_admin: &'a str,
}

/// Fail-closed check of a UserService_AcceptDvpProposal: every expected value is
/// compared, and an unreadable field or an unexpected node refuses the transaction.
fn check_accept_dvp(
    prepared: &PreparedTransaction,
    party: &str,
    proposal_id: &str,
    dvp_proposal_cid: &str,
    delivery: &ExpectedLeg<'_>,
    payment: &ExpectedLeg<'_>,
) -> Result<InspectionResult> {
    // --- Every expected value is given, so no check is skipped ---
    for (name, value) in [
        ("dvp_proposal_cid", dvp_proposal_cid),
        ("expected_delivery_amount", delivery.amount),
        ("expected_payment_amount", payment.amount),
        ("expected_delivery_instrument_id", delivery.instrument_id),
        (
            "expected_delivery_instrument_admin",
            delivery.instrument_admin,
        ),
        ("expected_payment_instrument_id", payment.instrument_id),
        (
            "expected_payment_instrument_admin",
            payment.instrument_admin,
        ),
    ] {
        if value.is_empty() {
            return Ok(refused(
                format!("AcceptDvp: empty {}", name),
                format!("{} is empty: AcceptDvp checks every expected value", name),
            ));
        }
    }

    let parts = match extract_root_exercise(prepared, "AcceptDvp") {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };
    let root = parts.root_exercise;

    // --- Single root: nothing may ride along with the accept ---
    if parts.tx.roots.len() != 1 {
        return Ok(refused(
            format!("AcceptDvp: expected 1 root, got {}", parts.tx.roots.len()),
            format!("Expected 1 root node, got {}", parts.tx.roots.len()),
        ));
    }

    // --- The acceptor alone submits and acts ---
    if parts.submitter.act_as != [party] {
        return Ok(refused(
            "AcceptDvp: acceptor not alone in act_as".to_string(),
            format!(
                "act_as {:?}, expected exactly [{}]",
                parts.submitter.act_as, party
            ),
        ));
    }
    if root.acting_parties != [party] {
        return Ok(refused(
            "AcceptDvp: acceptor not alone in acting_parties".to_string(),
            format!(
                "acting_parties {:?}, expected exactly [{}]",
                root.acting_parties, party
            ),
        ));
    }

    // --- Template: UserService ---
    let Some(tid) = &root.template_id else {
        return Ok(refused(
            "AcceptDvp: root has no template".to_string(),
            "Root exercise has no template_id, expected UserService".to_string(),
        ));
    };
    if !template_matches(tid, "Utility.Settlement.App.V1.Service.User", "UserService") {
        return Ok(refused(
            format!(
                "AcceptDvp: wrong template: {}.{}",
                tid.module_name, tid.entity_name
            ),
            format!(
                "Expected UserService template, got {}.{}",
                tid.module_name, tid.entity_name
            ),
        ));
    }

    // --- Metadata: command_id ---
    let expected_cmd = format!("dvp-accept-{}", proposal_id);
    if parts.submitter.command_id != expected_cmd {
        return Ok(refused(
            "AcceptDvp: wrong command_id".to_string(),
            format!(
                "command_id mismatch: expected={}, got={}",
                expected_cmd, parts.submitter.command_id
            ),
        ));
    }

    // --- Root exercise: choice_id ---
    if root.choice_id != "UserService_AcceptDvpProposal" {
        return Ok(refused(
            format!("AcceptDvp: wrong choice_id: {}", root.choice_id),
            format!(
                "Expected choice_id=UserService_AcceptDvpProposal, got={}",
                root.choice_id
            ),
        ));
    }

    // --- chosen_value: cid is the expected DvpProposal ---
    let cid = match accepted_cid(root) {
        Ok(cid) => cid,
        Err(reason) => return Ok(refused("AcceptDvp: unreadable cid".to_string(), reason)),
    };
    if cid != dvp_proposal_cid {
        return Ok(refused(
            "AcceptDvp: wrong cid".to_string(),
            format!(
                "chosen_value.cid={} does not match expected dvp_proposal_cid={}",
                short(cid, 16),
                short(dvp_proposal_cid, 16)
            ),
        ));
    }

    // --- DVP terms from the DvpProposal input contract with that cid ---
    let proposal = match dvp_proposal_input(parts.metadata, cid) {
        Ok(c) => c,
        Err(reason) => {
            return Ok(refused(
                "AcceptDvp: DvpProposal input not verified".to_string(),
                reason,
            ));
        }
    };
    let Some(terms) = proposal
        .argument
        .as_ref()
        .and_then(as_record)
        .and_then(|r| get_record_field(r, "terms"))
        .and_then(as_record)
    else {
        return Ok(refused(
            "AcceptDvp: unreadable DvpProposal terms".to_string(),
            "Could not extract terms from DvpProposal".to_string(),
        ));
    };
    let Some(terms_id) = get_record_field(terms, "id").and_then(get_text) else {
        return Ok(refused(
            "AcceptDvp: unreadable DvpProposal terms".to_string(),
            "Could not extract terms.id from DvpProposal".to_string(),
        ));
    };
    if terms_id != proposal_id {
        return Ok(refused(
            "AcceptDvp: DvpProposal has wrong terms.id".to_string(),
            format!(
                "DvpProposal.terms.id={}, expected={}",
                terms_id, proposal_id
            ),
        ));
    }

    for (leg, list, expected) in [
        ("delivery", "deliveries", delivery),
        ("payment", "payments", payment),
    ] {
        let (id, admin, amount) = match single_instrument_quantity(terms, list) {
            Ok(iq) => iq,
            Err(reason) => {
                return Ok(refused(
                    format!("AcceptDvp: cannot verify {}", list),
                    reason,
                ));
            }
        };
        if !numeric_eq(amount, expected.amount) {
            return Ok(refused(
                format!("AcceptDvp: {} amount mismatch", leg),
                format!("DVP {}={}, expected={}", leg, amount, expected.amount),
            ));
        }
        if id != expected.instrument_id {
            return Ok(refused(
                format!("AcceptDvp: {} instrument_id mismatch", leg),
                format!(
                    "DVP {} instrument.id={}, expected={}",
                    leg, id, expected.instrument_id
                ),
            ));
        }
        if admin != expected.instrument_admin {
            return Ok(refused(
                format!("AcceptDvp: {} instrument_admin mismatch", leg),
                format!(
                    "DVP {} instrument.admin={}, expected={}",
                    leg,
                    short(admin, 20),
                    short(expected.instrument_admin, 20)
                ),
            ));
        }
    }

    // --- Nodes: only what UserService_AcceptDvpProposal does in the Daml model ---
    if let Err(rejection) = check_accept_dvp_nodes(&parts, cid) {
        return Ok(rejection);
    }

    let summary = format!(
        "AcceptDvp VERIFIED: acceptor {} accepts proposal {} | {}",
        short(party, 8),
        proposal_id,
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings: vec![],
        rejection_reason: None,
    })
}

/// The `cid` of the UserService_AcceptDvpProposal choice argument.
fn accepted_cid(root: &Exercise) -> Result<&str, String> {
    let chosen = root
        .chosen_value
        .as_ref()
        .and_then(as_record)
        .ok_or_else(|| "Could not parse chosen_value as Record".to_string())?;
    let cid = get_record_field(chosen, "cid")
        .ok_or_else(|| "Could not extract chosen_value.cid".to_string())?;
    get_contract_id(cid).ok_or_else(|| "chosen_value.cid is not a contract id".to_string())
}

/// The only input contract with contract id `cid`, which must be a DvpProposal.
fn dvp_proposal_input<'a>(metadata: &'a Metadata, cid: &str) -> Result<&'a Create, String> {
    let found: Vec<&Create> = metadata
        .input_contracts
        .iter()
        .filter_map(|ic| match &ic.contract {
            Some(InputContractOneof::V1(c)) if c.contract_id == cid => Some(c),
            _ => None,
        })
        .collect();
    let [proposal] = found.as_slice() else {
        return Err(if found.is_empty() {
            format!(
                "No DvpProposal found in input_contracts to verify terms: none has contract_id {}",
                short(cid, 16)
            )
        } else {
            format!(
                "{} input contracts have contract_id {}",
                found.len(),
                short(cid, 16)
            )
        });
    };
    match &proposal.template_id {
        Some(t) if template_matches(t, DVP_MODULE, "DvpProposal") => Ok(*proposal),
        Some(t) => Err(format!(
            "Input contract {} is {}.{}, expected DvpProposal",
            short(cid, 16),
            t.module_name,
            t.entity_name
        )),
        None => Err(format!(
            "Input contract {} has no template_id",
            short(cid, 16)
        )),
    }
}

/// The instrument id, admin and amount of the only entry of `terms.<list>`.
fn single_instrument_quantity<'a>(
    terms: &'a Record,
    list: &str,
) -> Result<(&'a str, &'a str, &'a str), String> {
    let entries = get_record_field(terms, list)
        .and_then(as_list)
        .ok_or_else(|| format!("Could not extract {} from DvpProposal", list))?;
    let [entry] = entries else {
        return Err(format!(
            "DvpProposal has {} {}, expected exactly 1",
            entries.len(),
            list
        ));
    };
    let iq = as_record(entry)
        .ok_or_else(|| format!("Could not extract {}[0] from DvpProposal", list))?;
    let (id, admin, amount) = extract_instrument_quantity(iq);
    let id = id.ok_or_else(|| {
        format!(
            "Could not extract {}[0].instrument.id from DvpProposal",
            list
        )
    })?;
    let admin = admin.ok_or_else(|| {
        format!(
            "Could not extract {}[0].instrument.admin from DvpProposal",
            list
        )
    })?;
    let amount =
        amount.ok_or_else(|| format!("Could not extract {}[0].amount from DvpProposal", list))?;
    Ok((id, admin, amount))
}

/// Every node is the root, a fetch, the one DvpProposal_Accept of `cid` under the root or
/// the one Dvp that exercise creates; each node is listed once and reachable from the root.
fn check_accept_dvp_nodes(parts: &TxParts<'_>, cid: &str) -> Result<(), InspectionResult> {
    let tx = parts.tx;
    let Some(root_id) = tx.roots.first().map(String::as_str) else {
        return Err(refused(
            "AcceptDvp: no root nodes".to_string(),
            "Transaction has no root nodes".to_string(),
        ));
    };
    if parts.nodes_dict.len() != tx.nodes.len() {
        return Err(refused(
            "AcceptDvp: duplicate node ids".to_string(),
            "Node ids are not unique".to_string(),
        ));
    }

    // The parent of each node reachable from the root.
    let mut parents: HashMap<&str, &str> = HashMap::new();
    let mut pending = vec![root_id];
    while let Some(id) = pending.pop() {
        let Some(node) = parts.nodes_dict.get(id).copied() else {
            return Err(refused(
                format!("AcceptDvp: missing node {}", id),
                format!("Node {} is listed but not in the transaction", id),
            ));
        };
        let children: &[String] = match get_node_type(node) {
            Some(NodeType::Exercise(ex)) => &ex.children,
            Some(NodeType::Rollback(rb)) => &rb.children,
            _ => &[],
        };
        for child in children {
            if child.as_str() == root_id || parents.insert(child.as_str(), id).is_some() {
                return Err(refused(
                    format!("AcceptDvp: node {} listed twice", child),
                    format!("Node {} is listed more than once", child),
                ));
            }
            pending.push(child.as_str());
        }
    }

    let mut accept: Option<&str> = None;
    let mut dvp_create: Option<(&str, &str)> = None;
    for node in &tx.nodes {
        let id = node.node_id.as_str();
        if id == root_id {
            continue;
        }
        let Some(&parent) = parents.get(id) else {
            return Err(refused(
                format!("AcceptDvp: unreachable node {}", id),
                format!(
                    "Node {} ({}) is not reachable from the root",
                    id,
                    describe_node(node)
                ),
            ));
        };
        let second = match get_node_type(node) {
            Some(NodeType::Fetch(_)) => None,
            Some(NodeType::Exercise(ex))
                if parent == root_id
                    && ex.choice_id == "DvpProposal_Accept"
                    && ex.contract_id == cid
                    && ex
                        .template_id
                        .as_ref()
                        .is_some_and(|t| template_matches(t, DVP_MODULE, "DvpProposal")) =>
            {
                accept.replace(id).map(|_| "DvpProposal_Accept exercise")
            }
            Some(NodeType::Create(c))
                if c.template_id
                    .as_ref()
                    .is_some_and(|t| template_matches(t, DVP_MODULE, "Dvp")) =>
            {
                dvp_create.replace((id, parent)).map(|_| "Dvp create")
            }
            _ => {
                return Err(refused(
                    format!("AcceptDvp: unexpected node {}", id),
                    format!(
                        "Node {} ({}) is not allowed in an AcceptDvp transaction",
                        id,
                        describe_node(node)
                    ),
                ));
            }
        };
        if let Some(what) = second {
            return Err(refused(
                format!("AcceptDvp: second {}", what),
                format!("Node {} is a second {}; exactly one is allowed", id, what),
            ));
        }
    }

    let Some(accept) = accept else {
        return Err(refused(
            "AcceptDvp: no DvpProposal_Accept".to_string(),
            format!(
                "No DvpProposal_Accept exercise of {} under the root",
                short(cid, 16)
            ),
        ));
    };
    match dvp_create {
        Some((_, parent)) if parent == accept => Ok(()),
        Some((id, _)) => Err(refused(
            format!("AcceptDvp: unexpected node {}", id),
            format!(
                "Dvp create {} is not a child of DvpProposal_Accept {}",
                id, accept
            ),
        )),
        None => Err(refused(
            "AcceptDvp: no Dvp create".to_string(),
            "DvpProposal_Accept creates no Dvp".to_string(),
        )),
    }
}

// ============================================================================
// Allocate inspection (Phase B)
// ============================================================================

fn check_allocate(
    prepared: &PreparedTransaction,
    party: &str,
    proposal_id: &str,
    _dvp_cid: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, "Allocate") {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Metadata: act_as ---
    if !parts.submitter.act_as.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "Allocate: party not in act_as".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain party {}",
                parts.submitter.act_as, party
            )),
        });
    }

    // --- Metadata: command_id ---
    if !parts.submitter.command_id.contains(proposal_id) {
        warnings.push(format!(
            "command_id {} does not contain proposal_id {}",
            parts.submitter.command_id, proposal_id
        ));
    }

    // --- Root exercise: choice_id should be AllocationFactory_Allocate ---
    if parts.root_exercise.choice_id != "AllocationFactory_Allocate" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("Allocate: wrong choice_id: {}", parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=AllocationFactory_Allocate, got={}",
                parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: ExternalPartyAmuletRules (implements AllocationFactory) ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "Splice.ExternalPartyAmuletRules", "ExternalPartyAmuletRules")
            && !template_matches(tid, "Utility.Registry.App.V0.Service.AllocationFactory", "AllocationFactory")
        {
            warnings.push(format!(
                "Unexpected template: {}.{} (expected ExternalPartyAmuletRules or AllocationFactory)",
                tid.module_name, tid.entity_name
            ));
        }
    }

    // --- acting_parties ---
    if !parts.root_exercise.acting_parties.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "Allocate: party not in acting_parties".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain party {}",
                parts.root_exercise.acting_parties, party
            )),
        });
    }

    let summary = format!(
        "Allocate VERIFIED: {} allocates for {} | {}",
        short(party, 8),
        proposal_id,
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::tests::{root_exercise, run, single_exercise, CID, P, P_LONG, Q, Q_LONG};
    use crate::test_support::*;

    #[test]
    fn propose_dvp_summary_with_multibyte_party() {
        let mut ex = root_exercise("UserService_ProposeDvp", &[P]);
        ex.template_id = Some(ident("Utility.Settlement.App.V1.Service.User", "UserService"));
        let r = run(
            &single_exercise(ex, &[P]),
            OperationExpectation::ProposeDvp {
                proposer: P.into(),
                counterparty: Q.into(),
                proposal_id: "prop".into(),
                synchronizer_id: String::new(),
                terms: None,
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("buyer €€€€€€€€ proposes prop"), "{}", r.summary);
    }

    /// The synchronizer warnings of an accepted ProposeDvp on `got` when `expected` is configured.
    fn propose_dvp_synchronizer_warnings(got: &str, expected: &str) -> Vec<String> {
        let mut ex = root_exercise("UserService_ProposeDvp", &[P]);
        ex.template_id = Some(ident(
            "Utility.Settlement.App.V1.Service.User",
            "UserService",
        ));
        let mut p = single_exercise(ex, &[P]);
        p.metadata.as_mut().unwrap().synchronizer_id = got.into();
        let r = run(
            &p,
            OperationExpectation::ProposeDvp {
                proposer: P.into(),
                counterparty: Q.into(),
                proposal_id: "prop".into(),
                synchronizer_id: expected.into(),
                terms: None,
            },
        );
        assert!(r.accepted, "{got} vs {expected}: {r:?}");
        r.warnings
            .into_iter()
            .filter(|w| w.contains("synchronizer_id"))
            .collect()
    }

    #[test]
    fn propose_dvp_compares_synchronizers_by_logical_id() {
        const S: &str = "global-domain::1220cc";
        let s_37_8 = format!("{S}::37-8");
        let s_36_7 = format!("{S}::36-7");
        for (got, expected) in [
            (s_37_8.as_str(), S),
            (S, S),
            (S, s_37_8.as_str()),
            (s_36_7.as_str(), s_37_8.as_str()),
            (s_37_8.as_str(), ""),
        ] {
            let w = propose_dvp_synchronizer_warnings(got, expected);
            assert!(w.is_empty(), "{got} vs {expected}: {w:?}");
        }
    }

    #[test]
    fn propose_dvp_still_warns_on_another_synchronizer() {
        const S: &str = "global-domain::1220cc";
        let twice = format!("{S}::36-7::37-8");
        for got in [
            "global-domain::1220dd::37-8",
            "global-domain::1220dd",
            twice.as_str(),
            "",
        ] {
            let w = propose_dvp_synchronizer_warnings(got, S);
            assert_eq!(w.len(), 1, "{got}: {w:?}");
            let needle = format!("expected={S}, got={got}");
            assert!(w.iter().all(|x| x.contains(&needle)), "{got}: {w:?}");
        }
    }

    #[test]
    fn dvp_proposal_gc_summary_with_multibyte_ids() {
        let mut ex = root_exercise("DvpProposal_Cancel", &[P]);
        ex.template_id = Some(ident("Utility.Settlement.App.V1.Model.Dvp", "DvpProposal"));
        ex.contract_id = CID.into();
        let r = run(
            &single_exercise(ex, &[P]),
            OperationExpectation::CancelDvpProposal {
                party: P.into(),
                dvp_proposal_cid: CID.into(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("party €€€€€€€€ archives €€€€€€€€€€€€€€€€ |"), "{}", r.summary);
    }

    #[test]
    fn allocate_summary_with_multibyte_party() {
        let ex = root_exercise("AllocationFactory_Allocate", &[P]);
        let r = run(
            &single_exercise(ex, &[P]),
            OperationExpectation::Allocate {
                party: P.into(),
                proposal_id: "prop".into(),
                dvp_cid: String::new(),
                terms: None,
                settlement_operator: None,
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("€€€€€€€€ allocates for prop"), "{}", r.summary);
    }

    const USER_SERVICE: &str = "Utility.Settlement.App.V1.Service.User";

    /// AcceptDvp by P of "prop" expecting [cid, delivery amount, payment amount,
    /// delivery id, delivery admin, payment id, payment admin].
    fn accept_expectation(values: [&str; 7]) -> OperationExpectation {
        let [
            cid,
            delivery_amount,
            payment_amount,
            delivery_id,
            delivery_admin,
            payment_id,
            payment_admin,
        ] = values;
        OperationExpectation::AcceptDvp {
            acceptor: P.into(),
            proposal_id: "prop".into(),
            dvp_proposal_cid: cid.into(),
            expected_delivery_amount: delivery_amount.into(),
            expected_payment_amount: payment_amount.into(),
            expected_delivery_instrument_id: delivery_id.into(),
            expected_delivery_instrument_admin: delivery_admin.into(),
            expected_payment_instrument_id: payment_id.into(),
            expected_payment_instrument_admin: payment_admin.into(),
            terms: None,
        }
    }

    fn accept_dvp_expectation(delivery_admin: &str, payment_admin: &str) -> OperationExpectation {
        accept_expectation([CID, "1", "2.5", "D", delivery_admin, "Q", payment_admin])
    }

    /// An InstrumentQuantity of the DvpProposal terms.
    fn iq(id: &str, admin: &str, amount: &str) -> Value {
        record(vec![
            (
                "instrument",
                record(vec![("id", text(id)), ("admin", party(admin))]),
            ),
            ("amount", numeric(amount)),
        ])
    }

    /// An accept as the Daml model runs it: root "0" fetches the DvpProposal ("1") and
    /// exercises DvpProposal_Accept on it ("2"), which creates the Dvp ("3").
    struct AcceptTx {
        /// The cid in the choice argument, also fetched and accepted.
        cid: String,
        /// The contract id of the DvpProposal input contract.
        input_cid: String,
        terms_id: String,
        deliveries: Value,
        payments: Value,
        /// Further nodes listed under the root.
        extra_children: Vec<Node>,
    }

    impl AcceptTx {
        fn new(cid: &str) -> Self {
            Self {
                cid: cid.into(),
                input_cid: cid.into(),
                terms_id: "prop".into(),
                deliveries: list(vec![iq("D", Q, "1.0")]),
                payments: list(vec![iq("Q", Q, "2.5")]),
                extra_children: vec![],
            }
        }

        fn build(self) -> PreparedTransaction {
            let mut children = ids(&["1", "2"]);
            children.extend(self.extra_children.iter().map(|n| n.node_id.clone()));
            let mut root = exercise("UserService_AcceptDvpProposal", &[P], children);
            root.template_id = Some(ident(USER_SERVICE, "UserService"));
            root.consuming = false;
            root.chosen_value = Some(record(vec![
                ("cid", contract_id(&self.cid)),
                ("payload", record(vec![])),
            ]));
            let mut accept = exercise("DvpProposal_Accept", &[P], ids(&["3"]));
            accept.template_id = Some(ident(DVP_MODULE, "DvpProposal"));
            accept.contract_id = self.cid.clone();
            accept.chosen_value = Some(record(vec![]));
            let terms = record(vec![
                ("id", text(&self.terms_id)),
                ("deliveries", self.deliveries),
                ("payments", self.payments),
            ]);
            let dvp = create(
                ident(DVP_MODULE, "Dvp"),
                record(vec![("terms", terms.clone())]),
                &[P],
            );
            let mut nodes = vec![
                exercise_node("0", root),
                fetch_node(
                    "1",
                    fetch(ident(DVP_MODULE, "DvpProposal"), &self.cid, &[P]),
                ),
                exercise_node("2", accept),
                create_node("3", dvp),
            ];
            nodes.extend(self.extra_children);
            let mut p = prepared(
                &["0"],
                nodes,
                vec![seed(0, 1), seed(2, 2), seed(3, 3)],
                &[P],
            );
            submitter_mut(&mut p).command_id = "dvp-accept-prop".into();
            let mut proposal = create(
                ident(DVP_MODULE, "DvpProposal"),
                record(vec![("terms", terms)]),
                &[P],
            );
            proposal.contract_id = self.input_cid;
            with_input_contract(p, proposal)
        }
    }

    fn accept_dvp_tx(delivery_admin: &str, payment_admin: &str) -> PreparedTransaction {
        AcceptTx {
            deliveries: list(vec![iq("D", delivery_admin, "1.0")]),
            payments: list(vec![iq("Q", payment_admin, "2.5")]),
            ..AcceptTx::new(CID)
        }
        .build()
    }

    /// The valid accept of CID, changed by `f`.
    fn accept_mutated(f: impl FnOnce(&mut PreparedTransaction)) -> PreparedTransaction {
        let mut p = AcceptTx::new(CID).build();
        f(&mut p);
        p
    }

    fn submitter_mut(
        p: &mut PreparedTransaction,
    ) -> &mut proto_v2::interactive::metadata::SubmitterInfo {
        p.metadata
            .as_mut()
            .unwrap()
            .submitter_info
            .as_mut()
            .unwrap()
    }

    fn nodes_mut(p: &mut PreparedTransaction) -> &mut Vec<Node> {
        &mut p.transaction.as_mut().unwrap().nodes
    }

    fn exercise_mut<'a>(p: &'a mut PreparedTransaction, id: &str) -> &'a mut Exercise {
        let node = nodes_mut(p).iter_mut().find(|n| n.node_id == id).unwrap();
        let daml_transaction::node::VersionedNode::V1(n) = node.versioned_node.as_mut().unwrap();
        match n.node_type.as_mut().unwrap() {
            NodeType::Exercise(ex) => ex,
            _ => panic!("node {id} is not an exercise"),
        }
    }

    /// The last input contract: the fixture's DvpProposal.
    fn input_mut(p: &mut PreparedTransaction) -> &mut Create {
        let ic = p
            .metadata
            .as_mut()
            .unwrap()
            .input_contracts
            .last_mut()
            .unwrap();
        let InputContractOneof::V1(c) = ic.contract.as_mut().unwrap();
        c
    }

    /// Adds `node` under the exercise `parent`.
    fn add_under(p: &mut PreparedTransaction, parent: &str, node: Node) {
        exercise_mut(p, parent).children.push(node.node_id.clone());
        nodes_mut(p).push(node);
    }

    /// A DvpProposal_Accept exercise of `cid` with no children.
    fn dvp_accept(cid: &str) -> Exercise {
        let mut ex = exercise("DvpProposal_Accept", &[P], vec![]);
        ex.template_id = Some(ident(DVP_MODULE, "DvpProposal"));
        ex.contract_id = cid.into();
        ex
    }

    #[track_caller]
    fn assert_refused(p: &PreparedTransaction, exp: OperationExpectation, needle: &str) {
        let r = run(p, exp);
        assert!(
            !r.accepted,
            "accepted, expected a refusal containing {needle:?}"
        );
        let reason = r.rejection_reason.unwrap_or_default();
        assert!(
            reason.contains(needle),
            "{reason:?} does not contain {needle:?}"
        );
        assert!(r.summary.starts_with("AcceptDvp: "), "{}", r.summary);
    }

    #[test]
    fn accept_dvp_summary_with_multibyte_ids() {
        let r = run(&AcceptTx::new(CID).build(), accept_dvp_expectation(Q, Q));
        assert!(r.accepted, "{r:?}");
        assert!(
            r.summary
                .contains("acceptor €€€€€€€€ accepts proposal prop"),
            "{}",
            r.summary
        );
        // Without recorded terms only the warning that they are not compared
        assert_eq!(r.warnings, vec![NO_TERMS.to_string()]);
    }

    #[test]
    fn accept_dvp_refuses_another_cid_with_multibyte_ids() {
        let other = format!("{CID}x");
        let p = AcceptTx {
            cid: other.clone(),
            input_cid: other,
            ..AcceptTx::new(CID)
        }
        .build();
        let r = run(&p, accept_dvp_expectation(Q, Q));
        assert!(!r.accepted);
        assert_eq!(
            r.rejection_reason.as_deref(),
            Some(
                "chosen_value.cid=€€€€€€€€€€€€€€€€ does not match expected dvp_proposal_cid=€€€€€€€€€€€€€€€€"
            )
        );
    }

    #[test]
    fn accept_dvp_admin_mismatch_with_multibyte_admins() {
        let r = run(
            &accept_dvp_tx(P_LONG, Q_LONG),
            accept_dvp_expectation(Q_LONG, Q_LONG),
        );
        assert!(!r.accepted);
        assert_eq!(
            r.rejection_reason.as_deref(),
            Some(
                "DVP delivery instrument.admin=€€€€€€€€::1220aa-ext, expected=₿₿₿₿₿₿₿₿::1220bb-ext"
            )
        );
        let r = run(
            &accept_dvp_tx(Q_LONG, P_LONG),
            accept_dvp_expectation(Q_LONG, Q_LONG),
        );
        assert!(!r.accepted);
        assert_eq!(
            r.rejection_reason.as_deref(),
            Some(
                "DVP payment instrument.admin=€€€€€€€€::1220aa-ext, expected=₿₿₿₿₿₿₿₿::1220bb-ext"
            )
        );
    }

    #[test]
    fn accept_dvp_refuses_an_empty_expected_value() {
        let p = AcceptTx::new(CID).build();
        let names = [
            "dvp_proposal_cid",
            "expected_delivery_amount",
            "expected_payment_amount",
            "expected_delivery_instrument_id",
            "expected_delivery_instrument_admin",
            "expected_payment_instrument_id",
            "expected_payment_instrument_admin",
        ];
        for (i, name) in names.into_iter().enumerate() {
            let mut values = [CID, "1", "2.5", "D", Q, "Q", Q];
            values[i] = "";
            assert_refused(&p, accept_expectation(values), &format!("{name} is empty"));
        }
    }

    #[test]
    fn accept_dvp_refuses_another_submitter_or_actor() {
        for act_as in [ids(&[P, Q]), ids(&[Q]), vec![]] {
            let p = accept_mutated(|p| submitter_mut(p).act_as = act_as);
            assert_refused(&p, accept_dvp_expectation(Q, Q), "act_as [");
        }
        for acting in [ids(&[P, Q]), ids(&[Q]), vec![]] {
            let p = accept_mutated(|p| exercise_mut(p, "0").acting_parties = acting);
            assert_refused(&p, accept_dvp_expectation(Q, Q), "acting_parties [");
        }
    }

    #[test]
    fn accept_dvp_refuses_another_template_command_or_choice() {
        let exp = || accept_dvp_expectation(Q, Q);
        let p = accept_mutated(|p| exercise_mut(p, "0").template_id = None);
        assert_refused(&p, exp(), "Root exercise has no template_id");
        let p =
            accept_mutated(|p| exercise_mut(p, "0").template_id = Some(ident("M", "UserService")));
        assert_refused(
            &p,
            exp(),
            "Expected UserService template, got M.UserService",
        );
        let p = accept_mutated(|p| submitter_mut(p).command_id = "dvp-accept-other".into());
        assert_refused(
            &p,
            exp(),
            "command_id mismatch: expected=dvp-accept-prop, got=dvp-accept-other",
        );
        let p = accept_mutated(|p| {
            exercise_mut(p, "0").choice_id = "UserService_RejectDvpProposal".into()
        });
        assert_refused(
            &p,
            exp(),
            "Expected choice_id=UserService_AcceptDvpProposal, got=UserService_RejectDvpProposal",
        );
    }

    #[test]
    fn accept_dvp_refuses_an_unreadable_cid() {
        for (chosen, reason) in [
            (boolean(true), "Could not parse chosen_value as Record"),
            (
                record(vec![("payload", record(vec![]))]),
                "Could not extract chosen_value.cid",
            ),
            (
                record(vec![("cid", text(CID))]),
                "chosen_value.cid is not a contract id",
            ),
        ] {
            let p = accept_mutated(|p| exercise_mut(p, "0").chosen_value = Some(chosen));
            assert_refused(&p, accept_dvp_expectation(Q, Q), reason);
        }
    }

    #[test]
    fn accept_dvp_refuses_a_missing_duplicate_or_foreign_proposal() {
        let exp = || accept_dvp_expectation(Q, Q);
        let p = AcceptTx {
            input_cid: format!("{CID}x"),
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(
            &p,
            exp(),
            "No DvpProposal found in input_contracts to verify terms",
        );
        let p = accept_mutated(|p| {
            let inputs = &mut p.metadata.as_mut().unwrap().input_contracts;
            let twin = inputs[0].clone();
            inputs.push(twin);
        });
        assert_refused(&p, exp(), "2 input contracts have contract_id");
        let p = accept_mutated(|p| input_mut(p).template_id = Some(ident(DVP_MODULE, "Dvp")));
        assert_refused(
            &p,
            exp(),
            "is Utility.Settlement.App.V1.Model.Dvp.Dvp, expected DvpProposal",
        );
        let p = accept_mutated(|p| input_mut(p).template_id = None);
        assert_refused(&p, exp(), "has no template_id");
    }

    #[test]
    fn accept_dvp_ignores_a_decoy_proposal_listed_first() {
        let decoy_terms = record(vec![
            ("id", text("prop")),
            ("deliveries", list(vec![iq("D", Q, "9.0")])),
            ("payments", list(vec![iq("Q", Q, "2.5")])),
        ]);
        let mut decoy = create(
            ident(DVP_MODULE, "DvpProposal"),
            record(vec![("terms", decoy_terms)]),
            &[P],
        );
        decoy.contract_id = format!("{CID}x");
        let mut p = with_input_contract(AcceptTx::new(CID).build(), decoy);
        p.metadata.as_mut().unwrap().input_contracts.rotate_right(1);
        let r = run(&p, accept_dvp_expectation(Q, Q));
        assert!(r.accepted, "{r:?}");
    }

    #[test]
    fn accept_dvp_refuses_unreadable_terms() {
        fn quantity(instrument: Vec<(&str, Value)>, amount: Option<&str>) -> Value {
            let mut fields = vec![("instrument", record(instrument))];
            fields.extend(amount.map(|a| ("amount", numeric(a))));
            record(fields)
        }
        let exp = || accept_dvp_expectation(Q, Q);
        let leg = || list(vec![iq("D", Q, "1.0")]);
        for (argument, reason) in [
            (boolean(true), "Could not extract terms from DvpProposal"),
            (record(vec![]), "Could not extract terms from DvpProposal"),
            (
                record(vec![("terms", text("prop"))]),
                "Could not extract terms from DvpProposal",
            ),
            (
                record(vec![(
                    "terms",
                    record(vec![("deliveries", leg()), ("payments", leg())]),
                )]),
                "Could not extract terms.id from DvpProposal",
            ),
            (
                record(vec![(
                    "terms",
                    record(vec![("id", party("prop")), ("deliveries", leg())]),
                )]),
                "Could not extract terms.id from DvpProposal",
            ),
        ] {
            let p = accept_mutated(|p| input_mut(p).argument = Some(argument));
            assert_refused(&p, exp(), reason);
        }
        let named = || vec![("id", text("D")), ("admin", party(Q))];
        for (deliveries, reason) in [
            (text("D"), "Could not extract deliveries from DvpProposal"),
            (
                list(vec![text("D")]),
                "Could not extract deliveries[0] from DvpProposal",
            ),
            (
                list(vec![quantity(vec![("admin", party(Q))], Some("1.0"))]),
                "Could not extract deliveries[0].instrument.id from DvpProposal",
            ),
            (
                list(vec![quantity(vec![("id", text("D"))], Some("1.0"))]),
                "Could not extract deliveries[0].instrument.admin from DvpProposal",
            ),
            (
                list(vec![quantity(named(), None)]),
                "Could not extract deliveries[0].amount from DvpProposal",
            ),
        ] {
            assert_refused(
                &AcceptTx {
                    deliveries,
                    ..AcceptTx::new(CID)
                }
                .build(),
                exp(),
                reason,
            );
        }
        let p = AcceptTx {
            payments: list(vec![quantity(named(), None)]),
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(
            &p,
            exp(),
            "Could not extract payments[0].amount from DvpProposal",
        );
    }

    #[test]
    fn accept_dvp_refuses_another_terms_id() {
        let p = AcceptTx {
            terms_id: "other".into(),
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(
            &p,
            accept_dvp_expectation(Q, Q),
            "DvpProposal.terms.id=other, expected=prop",
        );
    }

    #[test]
    fn accept_dvp_refuses_other_than_one_delivery_and_one_payment() {
        let two = |id| list(vec![iq(id, Q, "1.0"), iq(id, Q, "1.0")]);
        for (tx, reason) in [
            (
                AcceptTx {
                    deliveries: list(vec![]),
                    ..AcceptTx::new(CID)
                },
                "DvpProposal has 0 deliveries, expected exactly 1",
            ),
            (
                AcceptTx {
                    deliveries: two("D"),
                    ..AcceptTx::new(CID)
                },
                "DvpProposal has 2 deliveries, expected exactly 1",
            ),
            (
                AcceptTx {
                    payments: list(vec![]),
                    ..AcceptTx::new(CID)
                },
                "DvpProposal has 0 payments, expected exactly 1",
            ),
            (
                AcceptTx {
                    payments: two("Q"),
                    ..AcceptTx::new(CID)
                },
                "DvpProposal has 2 payments, expected exactly 1",
            ),
        ] {
            assert_refused(&tx.build(), accept_dvp_expectation(Q, Q), reason);
        }
    }

    #[test]
    fn accept_dvp_compares_every_amount_and_instrument() {
        let exp = || accept_dvp_expectation(Q, Q);
        let p = AcceptTx {
            deliveries: list(vec![iq("D", Q, "1.5")]),
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), "DVP delivery=1.5, expected=1");
        let p = AcceptTx {
            payments: list(vec![iq("Q", Q, "3")]),
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), "DVP payment=3, expected=2.5");
        let p = AcceptTx {
            deliveries: list(vec![iq("Q", Q, "1.0")]),
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), "DVP delivery instrument.id=Q, expected=D");
        let p = AcceptTx {
            payments: list(vec![iq("D", Q, "2.5")]),
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), "DVP payment instrument.id=D, expected=Q");
        let p = AcceptTx {
            payments: list(vec![iq("Q", Q, "2.5000000000")]),
            ..AcceptTx::new(CID)
        }
        .build();
        assert!(run(&p, exp()).accepted);
    }

    #[test]
    fn accept_dvp_refuses_an_extra_exercise() {
        let exp = || accept_dvp_expectation(Q, Q);
        let accept_on =
            "Exercise DvpProposal_Accept on Utility.Settlement.App.V1.Model.Dvp.DvpProposal";
        let transfer = exercise_node("4", exercise("TransferFactory_Transfer", &[P], vec![]));
        let p = AcceptTx {
            extra_children: vec![transfer],
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(
            &p,
            exp(),
            "Node 4 (Exercise TransferFactory_Transfer on M.T) is not allowed",
        );
        let p = accept_mutated(|p| {
            add_under(p, "2", exercise_node("4", exercise("Other", &[P], vec![])))
        });
        assert_refused(&p, exp(), "Node 4 (Exercise Other on M.T) is not allowed");
        let p = AcceptTx {
            extra_children: vec![exercise_node("4", dvp_accept(CID))],
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), "Node 4 is a second DvpProposal_Accept exercise");
        // Look-alikes: another cid, another template module, or below the root's children
        let p = AcceptTx {
            extra_children: vec![exercise_node("4", dvp_accept("00ff"))],
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), &format!("Node 4 ({accept_on}) is not allowed"));
        let p =
            accept_mutated(|p| exercise_mut(p, "2").template_id = Some(ident("M", "DvpProposal")));
        assert_refused(
            &p,
            exp(),
            "Node 2 (Exercise DvpProposal_Accept on M.DvpProposal) is not allowed",
        );
        let p = accept_mutated(|p| add_under(p, "2", exercise_node("4", dvp_accept(CID))));
        assert_refused(&p, exp(), &format!("Node 4 ({accept_on}) is not allowed"));
    }

    #[test]
    fn accept_dvp_refuses_a_second_or_misplaced_create() {
        let exp = || accept_dvp_expectation(Q, Q);
        let dvp = |id| create_node(id, create(ident(DVP_MODULE, "Dvp"), record(vec![]), &[P]));
        let p = accept_mutated(|p| add_under(p, "2", dvp("4")));
        assert_refused(&p, exp(), "Node 4 is a second Dvp create");
        let p = AcceptTx {
            extra_children: vec![dvp("4")],
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), "Node 4 is a second Dvp create");
        let p = accept_mutated(|p| {
            add_under(
                p,
                "2",
                create_node("4", create(ident("M", "T"), record(vec![]), &[P])),
            )
        });
        assert_refused(&p, exp(), "Node 4 (Create of M.T) is not allowed");
        let p = accept_mutated(|p| {
            exercise_mut(p, "2").children.clear();
            exercise_mut(p, "0").children.push("3".into());
        });
        assert_refused(
            &p,
            exp(),
            "Dvp create 3 is not a child of DvpProposal_Accept 2",
        );
        let p = accept_mutated(|p| {
            exercise_mut(p, "2").children.clear();
            nodes_mut(p).retain(|n| n.node_id != "3");
        });
        assert_refused(&p, exp(), "DvpProposal_Accept creates no Dvp");
    }

    #[test]
    fn accept_dvp_refuses_a_rollback_unreachable_missing_or_repeated_node() {
        let exp = || accept_dvp_expectation(Q, Q);
        let p = AcceptTx {
            extra_children: vec![rollback_node("4", vec![])],
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), "Node 4 (Rollback) is not allowed");
        let typeless = daml_transaction::Node { node_id: "4".into(), versioned_node: None };
        let p = AcceptTx {
            extra_children: vec![typeless],
            ..AcceptTx::new(CID)
        }
        .build();
        assert_refused(&p, exp(), "Node 4 (no node type) is not allowed");
        let stray = fetch_node("4", fetch(ident(DVP_MODULE, "DvpProposal"), CID, &[P]));
        let p = accept_mutated(|p| nodes_mut(p).push(stray));
        assert_refused(
            &p,
            exp(),
            "Node 4 (Fetch of Utility.Settlement.App.V1.Model.Dvp.DvpProposal) is not reachable from the root",
        );
        let p = accept_mutated(|p| exercise_mut(p, "0").children.push("9".into()));
        assert_refused(&p, exp(), "Node 9 is listed but not in the transaction");
        let p = accept_mutated(|p| exercise_mut(p, "0").children.push("1".into()));
        assert_refused(&p, exp(), "Node 1 is listed more than once");
        let p = accept_mutated(|p| exercise_mut(p, "2").children.push("0".into()));
        assert_refused(&p, exp(), "Node 0 is listed more than once");
        let p = accept_mutated(|p| {
            let twin = nodes_mut(p)[1].clone();
            nodes_mut(p).push(twin);
        });
        assert_refused(&p, exp(), "Node ids are not unique");
        let p = accept_mutated(|p| {
            exercise_mut(p, "0").children = ids(&["1"]);
            nodes_mut(p).retain(|n| n.node_id == "0" || n.node_id == "1");
        });
        assert_refused(&p, exp(), "No DvpProposal_Accept exercise of");
    }

    #[test]
    fn accept_dvp_allows_an_extra_fetch_under_the_root() {
        let user_service = fetch_node("4", fetch(ident(USER_SERVICE, "UserService"), "00", &[P]));
        let p = AcceptTx {
            extra_children: vec![user_service],
            ..AcceptTx::new(CID)
        }
        .build();
        let r = run(&p, accept_dvp_expectation(Q, Q));
        assert!(r.accepted, "{r:?}");
        assert!(
            r.summary.contains("(2 exercise, 2 fetch, 1 create)"),
            "{}",
            r.summary
        );
    }

    #[test]
    fn only_accept_dvp_among_the_dvp_operations_requires_a_hash_match() {
        assert!(accept_dvp_expectation(Q, Q).requires_hash_match());
        for exp in [
            OperationExpectation::ProposeDvp {
                proposer: P.into(),
                counterparty: Q.into(),
                proposal_id: "prop".into(),
                synchronizer_id: String::new(),
                terms: None,
            },
            OperationExpectation::Allocate {
                party: P.into(),
                proposal_id: "prop".into(),
                dvp_cid: String::new(),
                terms: None,
                settlement_operator: None,
            },
            OperationExpectation::CancelDvpProposal {
                party: P.into(),
                dvp_proposal_cid: CID.into(),
            },
            OperationExpectation::RejectDvpProposal {
                party: P.into(),
                dvp_proposal_cid: CID.into(),
            },
        ] {
            assert!(!exp.requires_hash_match(), "{exp:?}");
        }
    }

    #[test]
    fn a_hash_mismatch_or_failure_rejects_an_accept_dvp() {
        use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
        // The hasher needs hex contract ids.
        let cid = "00a1";
        let exp = accept_expectation([cid, "1", "2.5", "D", Q, "Q", Q]);
        let bytes = encode(&AcceptTx::new(cid).build());
        let computed = crate::hasher::compute_hash(&bytes, "V2").unwrap();
        assert_ne!(computed, [0u8; 32]);

        let ok =
            crate::verify_and_hash(&bytes, &BASE64.encode(computed), "V2", &exp, false).unwrap();
        assert!(ok.accepted, "{:?}", ok.rejection_reason);
        assert_eq!(ok.computed_hash, computed);

        let mismatch =
            crate::verify_and_hash(&bytes, &BASE64.encode([7u8; 32]), "V2", &exp, false).unwrap();
        assert!(!mismatch.accepted);
        assert!(mismatch.rejection_reason.unwrap().contains("Hash mismatch"));

        // An exercise without a seed cannot be hashed.
        let mut unseeded = AcceptTx::new(cid).build();
        unseeded.transaction.as_mut().unwrap().node_seeds.clear();
        let unseeded = encode(&unseeded);
        assert_eq!(
            crate::hasher::compute_hash(&unseeded, "V2").unwrap(),
            [0u8; 32]
        );
        let failed =
            crate::verify_and_hash(&unseeded, &BASE64.encode(computed), "V2", &exp, false).unwrap();
        assert!(!failed.accepted);
        assert!(
            failed
                .rejection_reason
                .unwrap()
                .contains("could not be computed")
        );
    }

    #[test]
    fn no_verify_setting_lets_an_accept_dvp_sign_an_unmatched_hash() {
        use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
        use crate::{VerifyMode, VerifySetting};
        let cid = "00a1";
        let exp = accept_expectation([cid, "1", "2.5", "D", Q, "Q", Q]);
        let bytes = encode(&AcceptTx::new(cid).build());
        for setting in [VerifySetting::Default, VerifySetting::Observe, VerifySetting::Enforce] {
            let mode = VerifyMode { strict_all: false, setting };
            let r = crate::verify_and_hash_with(&bytes, &BASE64.encode([7u8; 32]), "V2", &exp, mode, false).unwrap();
            assert!(!r.accepted, "{setting:?}");
            assert!(r.rejection_reason.unwrap().contains("Hash mismatch"), "{setting:?}");
            assert!(r.would_refuse.is_empty(), "{setting:?}");
        }
    }

    #[test]
    fn dvp_root_count_rejections() {
        let empty = prepared(&[], vec![], vec![], &[P]);
        for exp in [
            OperationExpectation::Allocate {
                party: P.into(),
                proposal_id: "prop".into(),
                dvp_cid: String::new(),
                terms: None,
                settlement_operator: None,
            },
            accept_dvp_expectation(Q, Q),
        ] {
            let r = run(&empty, exp);
            assert!(!r.accepted);
            assert_eq!(r.rejection_reason.as_deref(), Some("Transaction has no root nodes"));
        }
        let mut two_roots = AcceptTx::new(CID).build();
        two_roots.transaction.as_mut().unwrap().roots.push("1".into());
        let r = run(&two_roots, accept_dvp_expectation(Q, Q));
        assert!(!r.accepted);
        assert_eq!(r.summary, "AcceptDvp: expected 1 root, got 2");
        assert_eq!(r.rejection_reason.as_deref(), Some("Expected 1 root node, got 2"));
    }
}
