//! Whole-transaction facts read from prepared transaction bytes: root exercises,
//! value outflows, DvP terms, settle terms and templates outside the model.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use prost::Message;
pub use rust_decimal::Decimal;

use crate::decimal::parse_exact;
use crate::decode::canton_proto::com::daml::ledger::api::v2;
use crate::inspector::{
    as_list, as_record, get_contract_id, get_node_type, get_numeric, get_party, get_record_field,
    get_text,
};
use v2::interactive::metadata::input_contract::Contract as InputContract;
use v2::interactive::transaction::v1::node::NodeType;
use v2::interactive::transaction::v1::{Create, Exercise};
use v2::interactive::{DamlTransaction, Metadata, PreparedTransaction};
use v2::{Identifier, Record, Value, value::Sum};

/// Deepest node nesting followed (roots are level 1).
const MAX_NODE_DEPTH: usize = 256;
const AMULET_ID: &str = "Amulet";
const SETTLE_CHOICE: &str = "AtomicDVP_Settle";
const DVP_MODULE: &str = "Utility.Settlement.App.V1.Model.Dvp";

/// The facts of one prepared transaction, as seen by one party.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TxFacts {
    /// The submitting parties named in the metadata.
    pub act_as: Vec<String>,
    /// Every root exercise, in order.
    pub roots: Vec<ExerciseFact>,
    /// Holdings created for other parties, and the party's pending transfers,
    /// allocations, locks and burns, in node order.
    pub outflows: Vec<ValueFlow>,
    /// The terms of every created or input DvP contract, without repeats.
    pub dvp: Vec<DvpTerms>,
    /// The terms of the atomic settle the transaction runs, if any.
    pub atomic_settle: Option<SettleFacts>,
    /// Created or exercised templates outside the model, as `module:entity`, sorted.
    pub unmodelled: Vec<String>,
}

/// One root exercise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExerciseFact {
    /// `module:entity`.
    pub template: String,
    pub choice: String,
    pub contract_id: String,
}

/// Value that leaves the party.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueFlow {
    /// The receiver; the other lock holders (comma-separated) for a lock, the
    /// destination address for a burn.
    pub to: String,
    pub instrument_admin: String,
    pub instrument_id: String,
    pub amount: Decimal,
    pub kind: FlowKind,
}

/// How value leaves the party.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowKind {
    /// A holding created for another party.
    Holding,
    /// A transfer instruction or offer the party sends.
    PendingTransfer,
    /// An allocation of a transfer leg the party sends.
    Allocation,
    /// A holding of the party locked to other parties.
    Lock,
    /// A burn for a release outside the ledger.
    Burn,
}

/// An instrument and an amount.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Quantity {
    pub instrument_admin: String,
    pub instrument_id: String,
    pub amount: Decimal,
}

/// The terms of a DvP proposal or DvP.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DvpTerms {
    /// `terms.id`, when present.
    pub proposal_id: Option<String>,
    pub buyer: String,
    pub seller: String,
    pub delivery: Quantity,
    pub payment: Quantity,
}

/// The terms of an atomic settle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleFacts {
    pub venue_cid: String,
    pub quote_id: String,
    pub ticket_id: String,
    pub user: String,
    pub side: QuoteSide,
    pub base_amount: Decimal,
    pub quote_amount: Decimal,
    pub lp_input_holding_cids: Vec<String>,
    pub user_input_holding_cids: Vec<String>,
    /// From the venue input contract, when the transaction carries it.
    pub venue: Option<VenueTerms>,
}

/// The side of a settled quote, for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteSide {
    Buy,
    Sell,
}

/// The parties and instruments of a venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueTerms {
    pub lp: String,
    pub provider: String,
    pub pair_name: String,
    pub base: Instrument,
    pub quote: Instrument,
}

/// A CIP-56 instrument id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instrument {
    pub admin: String,
    pub id: String,
}

/// Why no facts could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactsError {
    /// The bytes are not a prepared transaction.
    Decode(String),
    /// The transaction or one of its records does not have the expected shape.
    Malformed(String),
}

impl fmt::Display for FactsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(reason) => write!(f, "undecodable prepared transaction: {reason}"),
            Self::Malformed(reason) => write!(f, "malformed prepared transaction: {reason}"),
        }
    }
}

impl std::error::Error for FactsError {}

/// Reads the facts of every root and node of a prepared transaction for `party`.
pub fn extract_facts(
    prepared_transaction_bytes: &[u8],
    party: &str,
) -> Result<TxFacts, FactsError> {
    let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
        .map_err(|e| FactsError::Decode(e.to_string()))?;
    let tx = prepared
        .transaction
        .as_ref()
        .ok_or_else(|| malformed("missing transaction"))?;
    let metadata = prepared
        .metadata
        .as_ref()
        .ok_or_else(|| malformed("missing metadata"))?;
    let visits = walk(tx)?;

    let mut facts = TxFacts {
        act_as: metadata
            .submitter_info
            .as_ref()
            .map(|s| s.act_as.clone())
            .unwrap_or_default(),
        ..TxFacts::default()
    };
    let mut unmodelled = BTreeSet::new();
    let mut dvp_seen = HashSet::new();
    let mut created = HashSet::new();
    let mut consumed = HashSet::new();
    let mut creates: Vec<(Model, &Create)> = Vec::new();
    for visit in &visits {
        match visit.node {
            NodeType::Create(c) => {
                if !created.insert(c.contract_id.as_str()) {
                    return Err(malformed(format!(
                        "contract id {} is created twice",
                        c.contract_id
                    )));
                }
                let (id, name) = template(c.template_id.as_ref())?;
                match model_of(id) {
                    None => {
                        unmodelled.insert(name);
                    }
                    Some(model) if !visit.rolled_back => creates.push((model, c)),
                    Some(_) => {}
                }
            }
            NodeType::Exercise(e) => {
                let (id, name) = template(e.template_id.as_ref())?;
                let model = model_of(id);
                if visit.root {
                    facts.roots.push(ExerciseFact {
                        template: name.clone(),
                        choice: e.choice_id.clone(),
                        contract_id: e.contract_id.clone(),
                    });
                }
                if model.is_none() {
                    unmodelled.insert(name);
                }
                if visit.rolled_back {
                    continue;
                }
                if e.consuming {
                    consumed.insert(e.contract_id.as_str());
                }
                if model == Some(Model::Venue) && e.choice_id == SETTLE_CHOICE {
                    if facts.atomic_settle.is_some() {
                        return Err(malformed("the transaction runs more than one settle"));
                    }
                    facts.atomic_settle = Some(settle_facts(e, metadata)?);
                }
            }
            NodeType::Fetch(_) | NodeType::Rollback(_) => {}
        }
    }

    for input in &metadata.input_contracts {
        let Some(InputContract::V1(c)) = &input.contract else {
            continue;
        };
        if let Some(model @ (Model::DvpProposal | Model::Dvp)) =
            c.template_id.as_ref().and_then(model_of)
        {
            push_dvp(
                &mut facts.dvp,
                &mut dvp_seen,
                dvp_terms(argument(c)?, model)?,
            );
        }
    }

    // A contract consumed in the same transaction moves value only through what
    // its consuming choice creates, which is read on its own.
    let mut backing = HashSet::new();
    let mut locks = Vec::new();
    for &(model, c) in &creates {
        let arg = argument(c)?;
        let transient = consumed.contains(c.contract_id.as_str());
        let flow = match model {
            Model::Amulet if !transient => amulet_holding(arg, party)?,
            Model::RegistryHolding if !transient => registry_holding(arg, party)?,
            Model::LockedAmulet if !transient => {
                locks.push((c.contract_id.as_str(), arg));
                None
            }
            Model::AmuletInstruction if !transient => {
                let flow = leg_flow(
                    record_field(arg, "transfer")?,
                    party,
                    FlowKind::PendingTransfer,
                )?;
                if flow.is_some() {
                    backing.insert(cid_field(arg, "lockedAmulet")?);
                }
                flow
            }
            Model::TransferOffer if !transient => leg_flow(
                record_field(arg, "transfer")?,
                party,
                FlowKind::PendingTransfer,
            )?,
            Model::RegistryTransferOffer if !transient => {
                registry_offer(record_field(arg, "transfer")?, party)?
            }
            Model::AmuletAllocation if !transient => {
                let spec = record_field(arg, "allocation")?;
                let flow = leg_flow(
                    record_field(spec, "transferLeg")?,
                    party,
                    FlowKind::Allocation,
                )?;
                if flow.is_some() {
                    backing.insert(cid_field(arg, "lockedAmulet")?);
                }
                flow
            }
            Model::LegAllocation if !transient => {
                let spec = record_field(arg, "allocation")?;
                leg_flow(
                    record_field(spec, "transferLeg")?,
                    party,
                    FlowKind::Allocation,
                )?
            }
            Model::WithdrawAttestation if !transient => burn(arg, party)?,
            Model::DvpProposal | Model::Dvp => {
                push_dvp(&mut facts.dvp, &mut dvp_seen, dvp_terms(arg, model)?);
                None
            }
            _ => None,
        };
        facts.outflows.extend(flow);
    }
    for (cid, arg) in locks {
        facts
            .outflows
            .extend(locked_amulet(arg, cid, party, &backing)?);
    }
    facts.unmodelled = unmodelled.into_iter().collect();
    Ok(facts)
}

// ============================================================================
// Node walk
// ============================================================================

struct Visit<'a> {
    node: &'a NodeType,
    root: bool,
    rolled_back: bool,
}

/// Every node reachable from the roots, in pre-order; each node exactly once.
fn walk(tx: &DamlTransaction) -> Result<Vec<Visit<'_>>, FactsError> {
    let mut index: HashMap<&str, &NodeType> = HashMap::with_capacity(tx.nodes.len());
    for node in &tx.nodes {
        let node_type = get_node_type(node)
            .ok_or_else(|| malformed(format!("node {} has no content", node.node_id)))?;
        if index.insert(node.node_id.as_str(), node_type).is_some() {
            return Err(malformed(format!("node id {} is used twice", node.node_id)));
        }
    }
    if tx.roots.is_empty() {
        return Err(malformed("the transaction has no root nodes"));
    }
    let no_children: &[String] = &[];
    let mut seen: HashSet<&str> = HashSet::with_capacity(index.len());
    let mut visits = Vec::with_capacity(index.len());
    let mut stack: Vec<(&str, usize, bool)> = tx
        .roots
        .iter()
        .rev()
        .map(|id| (id.as_str(), 1, false))
        .collect();
    while let Some((id, depth, rolled_back)) = stack.pop() {
        if depth > MAX_NODE_DEPTH {
            return Err(malformed("node nesting is too deep"));
        }
        if !seen.insert(id) {
            return Err(malformed(format!("node {id} is reached twice")));
        }
        let node = *index
            .get(id)
            .ok_or_else(|| malformed(format!("node {id} is missing")))?;
        visits.push(Visit {
            node,
            root: depth == 1,
            rolled_back,
        });
        let (children, rolled_back) = match node {
            NodeType::Exercise(e) => (e.children.as_slice(), rolled_back),
            NodeType::Rollback(r) => (r.children.as_slice(), true),
            NodeType::Create(_) | NodeType::Fetch(_) => (no_children, rolled_back),
        };
        let depth = depth.saturating_add(1);
        stack.extend(
            children
                .iter()
                .rev()
                .map(|c| (c.as_str(), depth, rolled_back)),
        );
    }
    if seen.len() != index.len() {
        return Err(malformed("the transaction has nodes no root reaches"));
    }
    Ok(visits)
}

// ============================================================================
// Templates
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Model {
    Amulet,
    LockedAmulet,
    RegistryHolding,
    AmuletInstruction,
    TransferOffer,
    RegistryTransferOffer,
    AmuletAllocation,
    LegAllocation,
    WithdrawAttestation,
    DvpProposal,
    Dvp,
    Venue,
    /// Moves value only through the contracts it creates.
    Neutral,
    /// Activity markers and reward coupons.
    Ignored,
}

const MODELS: &[(&str, &str, Model)] = &[
    ("Splice.Amulet", "Amulet", Model::Amulet),
    ("Splice.Amulet", "LockedAmulet", Model::LockedAmulet),
    (
        "Utility.Registry.Holding.V0.Holding",
        "Holding",
        Model::RegistryHolding,
    ),
    (
        "Splice.AmuletTransferInstruction",
        "AmuletTransferInstruction",
        Model::AmuletInstruction,
    ),
    (
        "Utility.Registry.App.V0.Model.Transfer",
        "TransferOffer",
        Model::TransferOffer,
    ),
    (
        "Utility.Registry.V0.Holding.Transfer",
        "TransferOffer",
        Model::RegistryTransferOffer,
    ),
    (
        "Splice.AmuletAllocation",
        "AmuletAllocation",
        Model::AmuletAllocation,
    ),
    (
        "Utility.Registry.V0.Holding.Allocation",
        "DvpLegAllocation",
        Model::LegAllocation,
    ),
    (
        "Utility.Bridge.V0.Attestation.Withdraw",
        "WithdrawAttestation",
        Model::WithdrawAttestation,
    ),
    (DVP_MODULE, "DvpProposal", Model::DvpProposal),
    (DVP_MODULE, "Dvp", Model::Dvp),
    ("AtomicDVP", "AtomicDVP", Model::Venue),
    (DVP_MODULE, "RejectedDvpProposal", Model::Neutral),
    (DVP_MODULE, "SettledDvp", Model::Neutral),
    (DVP_MODULE, "WithdrawnDvp", Model::Neutral),
    (DVP_MODULE, "RejectedDvp", Model::Neutral),
    (
        "Utility.Settlement.App.V1.Service.User",
        "UserService",
        Model::Neutral,
    ),
    ("AtomicDVP", "SettlementTicket", Model::Neutral),
    (
        "Splice.ExternalPartyAmuletRules",
        "ExternalPartyAmuletRules",
        Model::Neutral,
    ),
    ("Splice.AmuletRules", "AmuletRules", Model::Neutral),
    ("Splice.AmuletRules", "TransferPreapproval", Model::Neutral),
    ("Splice.Amulet", "FeaturedAppRight", Model::Neutral),
    (
        "Utility.Registry.V0.Rule.Transfer",
        "TransferFactory",
        Model::Neutral,
    ),
    (
        "Utility.Registry.V0.Rule.Transfer",
        "TransferRule",
        Model::Neutral,
    ),
    (
        "Utility.Registry.App.V0.Service.AllocationFactory",
        "AllocationFactory",
        Model::Neutral,
    ),
    (
        "Utility.Registry.App.V0.Model.TransferPreapproval",
        "TransferPreapproval",
        Model::Neutral,
    ),
    (
        "Utility.Bridge.V0.Agreement.User",
        "BridgeUserAgreement",
        Model::Neutral,
    ),
    ("MultiCall", "MultiCall", Model::Neutral),
    ("Splice.Amulet", "FeaturedAppActivityMarker", Model::Ignored),
    ("Splice.Amulet", "AppRewardCoupon", Model::Ignored),
    ("Splice.Amulet", "ValidatorRewardCoupon", Model::Ignored),
    ("Splice.Amulet", "SvRewardCoupon", Model::Ignored),
    ("Splice.Amulet", "RewardCouponV2", Model::Ignored),
    (
        "Splice.ValidatorLicense",
        "ValidatorFaucetCoupon",
        Model::Ignored,
    ),
    (
        "Splice.ValidatorLicense",
        "ValidatorLivenessActivityRecord",
        Model::Ignored,
    ),
];

fn model_of(id: &Identifier) -> Option<Model> {
    MODELS
        .iter()
        .find(|(module, entity, _)| id.module_name == *module && id.entity_name == *entity)
        .map(|&(_, _, model)| model)
}

fn template(id: Option<&Identifier>) -> Result<(&Identifier, String), FactsError> {
    let id = id.ok_or_else(|| malformed("a node has no template id"))?;
    Ok((id, format!("{}:{}", id.module_name, id.entity_name)))
}

// ============================================================================
// Flows
// ============================================================================

fn amulet_holding(arg: &Record, party: &str) -> Result<Option<ValueFlow>, FactsError> {
    let owner = party_field(arg, "owner")?;
    if owner == party {
        return Ok(None);
    }
    Ok(Some(ValueFlow {
        to: owner.to_string(),
        instrument_admin: party_field(arg, "dso")?.to_string(),
        instrument_id: AMULET_ID.to_string(),
        amount: amount_field(record_field(arg, "amount")?, "initialAmount")?,
        kind: FlowKind::Holding,
    }))
}

fn registry_holding(arg: &Record, party: &str) -> Result<Option<ValueFlow>, FactsError> {
    let owner = party_field(arg, "owner")?;
    if owner == party {
        return Ok(None);
    }
    let instrument = record_field(arg, "instrument")?;
    Ok(Some(ValueFlow {
        to: owner.to_string(),
        instrument_admin: party_field(instrument, "source")?.to_string(),
        instrument_id: text_field(instrument, "id")?.to_string(),
        amount: amount_field(arg, "amount")?,
        kind: FlowKind::Holding,
    }))
}

/// A CIP-56 `Transfer` or `TransferLeg` record that `party` sends.
fn leg_flow(leg: &Record, party: &str, kind: FlowKind) -> Result<Option<ValueFlow>, FactsError> {
    if party_field(leg, "sender")? != party {
        return Ok(None);
    }
    let instrument = record_field(leg, "instrumentId")?;
    Ok(Some(ValueFlow {
        to: party_field(leg, "receiver")?.to_string(),
        instrument_admin: party_field(instrument, "admin")?.to_string(),
        instrument_id: text_field(instrument, "id")?.to_string(),
        amount: amount_field(leg, "amount")?,
        kind,
    }))
}

fn registry_offer(transfer: &Record, party: &str) -> Result<Option<ValueFlow>, FactsError> {
    if party_field(transfer, "sender")? != party {
        return Ok(None);
    }
    let instrument = record_field(transfer, "instrumentIdentifier")?;
    Ok(Some(ValueFlow {
        to: party_field(transfer, "receiver")?.to_string(),
        instrument_admin: party_field(instrument, "source")?.to_string(),
        instrument_id: text_field(instrument, "id")?.to_string(),
        amount: amount_field(transfer, "amount")?,
        kind: FlowKind::PendingTransfer,
    }))
}

fn burn(arg: &Record, party: &str) -> Result<Option<ValueFlow>, FactsError> {
    if valid_party(arg, "sourceDepositor")? != party {
        return Ok(None);
    }
    let instrument = record_field(arg, "instrumentId")?;
    Ok(Some(ValueFlow {
        to: text_field(arg, "destinationRecipient")?.to_string(),
        instrument_admin: party_field(instrument, "admin")?.to_string(),
        instrument_id: text_field(instrument, "id")?.to_string(),
        amount: amount_field(arg, "amount")?,
        kind: FlowKind::Burn,
    }))
}

/// Another owner's locked amulet is a holding flow; the party's own is a lock flow
/// unless only its admin holds it for a pending transfer or allocation read above.
fn locked_amulet(
    arg: &Record,
    cid: &str,
    party: &str,
    backing: &HashSet<&str>,
) -> Result<Option<ValueFlow>, FactsError> {
    let amulet = record_field(arg, "amulet")?;
    let owner = party_field(amulet, "owner")?;
    let dso = party_field(amulet, "dso")?;
    let holders = list_field(record_field(arg, "lock")?, "holders")?
        .iter()
        .map(|h| get_party(h).ok_or_else(|| malformed("a lock holder is not a party")))
        .collect::<Result<BTreeSet<&str>, FactsError>>()?;
    let to = if owner != party {
        owner.to_string()
    } else if backing.contains(cid) && holders.len() == 1 && holders.contains(dso) {
        return Ok(None);
    } else {
        let others: Vec<&str> = holders.into_iter().filter(|h| *h != party).collect();
        if others.is_empty() {
            return Ok(None);
        }
        others.join(",")
    };
    let kind = if owner != party {
        FlowKind::Holding
    } else {
        FlowKind::Lock
    };
    Ok(Some(ValueFlow {
        to,
        instrument_admin: dso.to_string(),
        instrument_id: AMULET_ID.to_string(),
        amount: amount_field(record_field(amulet, "amount")?, "initialAmount")?,
        kind,
    }))
}

// ============================================================================
// DvP and settle terms
// ============================================================================

fn dvp_terms(arg: &Record, model: Model) -> Result<DvpTerms, FactsError> {
    let (buyer, seller) = if model == Model::DvpProposal {
        let proposer = party_field(arg, "proposer")?;
        let counterparty = party_field(arg, "counterparty")?;
        if bool_field(arg, "proposerIsBuyer")? {
            (proposer, counterparty)
        } else {
            (counterparty, proposer)
        }
    } else {
        (party_field(arg, "buyer")?, party_field(arg, "seller")?)
    };
    let terms = record_field(arg, "terms")?;
    let proposal_id = match get_record_field(terms, "id") {
        None => None,
        Some(v) => Some(
            get_text(v)
                .ok_or_else(|| malformed("terms.id is not a Text"))?
                .to_string(),
        ),
    };
    Ok(DvpTerms {
        proposal_id,
        buyer: buyer.to_string(),
        seller: seller.to_string(),
        delivery: single_quantity(terms, "deliveries")?,
        payment: single_quantity(terms, "payments")?,
    })
}

fn push_dvp(all: &mut Vec<DvpTerms>, seen: &mut HashSet<DvpTerms>, terms: DvpTerms) {
    if seen.insert(terms.clone()) {
        all.push(terms);
    }
}

/// The only entry of `terms.<list>`.
fn single_quantity(terms: &Record, list: &str) -> Result<Quantity, FactsError> {
    let entries = list_field(terms, list)?;
    let [entry] = entries else {
        return Err(malformed(format!(
            "terms.{list} has {} entries, expected 1",
            entries.len()
        )));
    };
    let quantity =
        as_record(entry).ok_or_else(|| malformed(format!("terms.{list} holds a non-record")))?;
    let instrument = record_field(quantity, "instrument")?;
    Ok(Quantity {
        instrument_admin: party_field(instrument, "admin")?.to_string(),
        instrument_id: text_field(instrument, "id")?.to_string(),
        amount: amount_field(quantity, "amount")?,
    })
}

fn settle_facts(e: &Exercise, metadata: &Metadata) -> Result<SettleFacts, FactsError> {
    let chosen = e
        .chosen_value
        .as_ref()
        .and_then(as_record)
        .ok_or_else(|| malformed("the settle argument is not a record"))?;
    let quote = record_field(chosen, "quote")?;
    let side = match constructor_field(quote, "side")? {
        "Buy" => QuoteSide::Buy,
        "Sell" => QuoteSide::Sell,
        other => return Err(malformed(format!("quote side {other} is unknown"))),
    };
    Ok(SettleFacts {
        venue_cid: e.contract_id.clone(),
        quote_id: text_field(quote, "quoteId")?.to_string(),
        ticket_id: text_field(quote, "ticketId")?.to_string(),
        user: party_field(quote, "user")?.to_string(),
        side,
        base_amount: amount_field(quote, "baseAmount")?,
        quote_amount: amount_field(quote, "quoteAmount")?,
        lp_input_holding_cids: cid_list(chosen, "lpInputHoldingCids")?,
        user_input_holding_cids: cid_list(chosen, "userInputHoldingCids")?,
        venue: venue_terms(metadata, &e.contract_id)?,
    })
}

fn venue_terms(metadata: &Metadata, cid: &str) -> Result<Option<VenueTerms>, FactsError> {
    let mut found = metadata
        .input_contracts
        .iter()
        .filter_map(|input| match &input.contract {
            Some(InputContract::V1(c)) if c.contract_id == cid => Some(c),
            _ => None,
        });
    let Some(venue) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(malformed("the venue is an input contract twice"));
    }
    let (id, name) = template(venue.template_id.as_ref())?;
    if model_of(id) != Some(Model::Venue) {
        return Err(malformed(format!("the settled venue is a {name}")));
    }
    let arg = argument(venue)?;
    Ok(Some(VenueTerms {
        lp: party_field(arg, "lp")?.to_string(),
        provider: party_field(arg, "provider")?.to_string(),
        pair_name: text_field(arg, "pairName")?.to_string(),
        base: instrument(record_field(arg, "baseInstrumentId")?)?,
        quote: instrument(record_field(arg, "quoteInstrumentId")?)?,
    }))
}

fn instrument(r: &Record) -> Result<Instrument, FactsError> {
    Ok(Instrument {
        admin: party_field(r, "admin")?.to_string(),
        id: text_field(r, "id")?.to_string(),
    })
}

// ============================================================================
// Field helpers
// ============================================================================

fn malformed(reason: impl Into<String>) -> FactsError {
    FactsError::Malformed(reason.into())
}

fn argument(c: &Create) -> Result<&Record, FactsError> {
    c.argument
        .as_ref()
        .and_then(as_record)
        .ok_or_else(|| malformed(format!("contract {} has no record argument", c.contract_id)))
}

fn field<'a>(r: &'a Record, label: &str) -> Result<&'a Value, FactsError> {
    get_record_field(r, label).ok_or_else(|| malformed(format!("missing field {label}")))
}

fn record_field<'a>(r: &'a Record, label: &str) -> Result<&'a Record, FactsError> {
    as_record(field(r, label)?).ok_or_else(|| malformed(format!("{label} is not a record")))
}

fn list_field<'a>(r: &'a Record, label: &str) -> Result<&'a [Value], FactsError> {
    as_list(field(r, label)?).ok_or_else(|| malformed(format!("{label} is not a list")))
}

fn party_field<'a>(r: &'a Record, label: &str) -> Result<&'a str, FactsError> {
    get_party(field(r, label)?).ok_or_else(|| malformed(format!("{label} is not a party")))
}

fn text_field<'a>(r: &'a Record, label: &str) -> Result<&'a str, FactsError> {
    get_text(field(r, label)?).ok_or_else(|| malformed(format!("{label} is not a Text")))
}

fn bool_field(r: &Record, label: &str) -> Result<bool, FactsError> {
    match field(r, label)?.sum.as_ref() {
        Some(Sum::Bool(b)) => Ok(*b),
        _ => Err(malformed(format!("{label} is not a Bool"))),
    }
}

fn cid_field<'a>(r: &'a Record, label: &str) -> Result<&'a str, FactsError> {
    get_contract_id(field(r, label)?)
        .ok_or_else(|| malformed(format!("{label} is not a contract id")))
}

fn cid_list(r: &Record, label: &str) -> Result<Vec<String>, FactsError> {
    list_field(r, label)?
        .iter()
        .map(|v| {
            get_contract_id(v)
                .map(str::to_string)
                .ok_or_else(|| malformed(format!("{label} holds a non contract id")))
        })
        .collect()
}

/// The constructor of an enum, or of a variant without payload use.
fn constructor_field<'a>(r: &'a Record, label: &str) -> Result<&'a str, FactsError> {
    match field(r, label)?.sum.as_ref() {
        Some(Sum::Enum(e)) => Ok(e.constructor.as_str()),
        Some(Sum::Variant(v)) => Ok(v.constructor.as_str()),
        _ => Err(malformed(format!("{label} is not an enum"))),
    }
}

/// The party of a `Valid party` variant.
fn valid_party<'a>(r: &'a Record, label: &str) -> Result<&'a str, FactsError> {
    match field(r, label)?.sum.as_ref() {
        Some(Sum::Variant(v)) if v.constructor == "Valid" => v
            .value
            .as_deref()
            .and_then(get_party)
            .ok_or_else(|| malformed(format!("{label} holds no party"))),
        _ => Err(malformed(format!("{label} is not a Valid party"))),
    }
}

/// A non-negative numeric, parsed exactly.
fn amount_field(r: &Record, label: &str) -> Result<Decimal, FactsError> {
    let text = get_numeric(field(r, label)?)
        .ok_or_else(|| malformed(format!("{label} is not a numeric")))?;
    let amount = parse_exact(text)
        .ok_or_else(|| malformed(format!("{label} {text} is not an exact decimal")))?;
    if amount < Decimal::ZERO {
        return Err(malformed(format!("{label} {text} is negative")));
    }
    Ok(amount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    const P: &str = "alice::1220aa";
    const Q: &str = "bob::1220bb";
    const X: &str = "eve::1220ee";
    const DSO: &str = "dso::1220dd";
    const REG: &str = "registrar::1220ff";
    const RULES: (&str, &str) = (
        "Splice.ExternalPartyAmuletRules",
        "ExternalPartyAmuletRules",
    );
    const USER_SERVICE: (&str, &str) = ("Utility.Settlement.App.V1.Service.User", "UserService");

    fn dec(s: &str) -> Decimal {
        parse_exact(s).unwrap()
    }

    fn flow(to: &str, admin: &str, id: &str, amount: &str, kind: FlowKind) -> ValueFlow {
        ValueFlow {
            to: to.into(),
            instrument_admin: admin.into(),
            instrument_id: id.into(),
            amount: dec(amount),
            kind,
        }
    }

    fn with_cid(mut c: Create, cid: &str) -> Create {
        c.contract_id = cid.into();
        c
    }

    fn instrument_id(admin: &str, id: &str) -> Value {
        record(vec![("admin", party(admin)), ("id", text(id))])
    }

    fn amulet_value(owner: &str, amount: &str) -> Value {
        record(vec![
            ("dso", party(DSO)),
            ("owner", party(owner)),
            ("amount", record(vec![("initialAmount", numeric(amount))])),
        ])
    }

    fn amulet(cid: &str, owner: &str, amount: &str) -> Create {
        with_cid(
            create(
                ident("Splice.Amulet", "Amulet"),
                amulet_value(owner, amount),
                &[DSO, owner],
            ),
            cid,
        )
    }

    fn locked(cid: &str, owner: &str, amount: &str, holders: &[&str]) -> Create {
        let lock = record(vec![
            ("holders", list(holders.iter().map(|h| party(h)).collect())),
            ("optContext", optional(None)),
        ]);
        let arg = record(vec![
            ("amulet", amulet_value(owner, amount)),
            ("lock", lock),
        ]);
        with_cid(
            create(ident("Splice.Amulet", "LockedAmulet"), arg, &[DSO, owner]),
            cid,
        )
    }

    fn leg(sender: &str, receiver: &str, amount: &str, admin: &str, id: &str) -> Value {
        record(vec![
            ("sender", party(sender)),
            ("receiver", party(receiver)),
            ("amount", numeric(amount)),
            ("instrumentId", instrument_id(admin, id)),
        ])
    }

    fn instruction(cid: &str, lock: &str, sender: &str, receiver: &str, amount: &str) -> Create {
        let arg = record(vec![
            ("lockedAmulet", contract_id(lock)),
            ("transfer", leg(sender, receiver, amount, DSO, "Amulet")),
        ]);
        let t = ident(
            "Splice.AmuletTransferInstruction",
            "AmuletTransferInstruction",
        );
        with_cid(create(t, arg, &[DSO, sender]), cid)
    }

    fn allocation(cid: &str, lock: &str, sender: &str, receiver: &str, amount: &str) -> Create {
        let spec = record(vec![
            ("transferLegId", text("leg0")),
            ("transferLeg", leg(sender, receiver, amount, DSO, "Amulet")),
        ]);
        let arg = record(vec![
            ("lockedAmulet", contract_id(lock)),
            ("allocation", spec),
        ]);
        with_cid(
            create(
                ident("Splice.AmuletAllocation", "AmuletAllocation"),
                arg,
                &[DSO, sender],
            ),
            cid,
        )
    }

    /// A nonconsuming exercise of `choice` on `template` by P.
    fn root(template: (&str, &str), choice: &str, children: &[&str]) -> Exercise {
        let mut ex = exercise(choice, &[P], ids(children));
        ex.template_id = Some(ident(template.0, template.1));
        ex.consuming = false;
        ex
    }

    /// One TransferFactory_Transfer root whose children are `creates`.
    fn transfer_tx(creates: Vec<Create>) -> PreparedTransaction {
        let child_ids: Vec<String> = (1..=creates.len()).map(|i| format!("n{i}")).collect();
        let mut ex = root(RULES, "TransferFactory_Transfer", &[]);
        ex.children = child_ids.clone();
        let mut nodes = vec![exercise_node("0", ex)];
        nodes.extend(
            child_ids
                .iter()
                .zip(creates)
                .map(|(id, c)| create_node(id, c)),
        );
        prepared(&["0"], nodes, vec![], &[P])
    }

    fn add_root(p: &mut PreparedTransaction, id: &str, ex: Exercise, nodes: Vec<Node>) {
        let tx = p.transaction.as_mut().unwrap();
        tx.roots.push(id.into());
        tx.nodes.push(exercise_node(id, ex));
        tx.nodes.extend(nodes);
    }

    fn facts(p: &PreparedTransaction) -> TxFacts {
        extract_facts(&encode(p), P).unwrap()
    }

    fn malformed_reason(p: &PreparedTransaction) -> String {
        match extract_facts(&encode(p), P) {
            Err(FactsError::Malformed(reason)) => reason,
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn amulet_to_receiver_plus_change_to_self_gives_one_holding_flow() {
        let marker = create(
            ident("Splice.Amulet", "FeaturedAppActivityMarker"),
            record(vec![]),
            &[DSO],
        );
        let f = facts(&transfer_tx(vec![
            amulet("c1", Q, "10.0000000000"),
            amulet("c2", P, "89.5000000000"),
            with_cid(marker, "m1"),
        ]));
        assert_eq!(
            f.outflows,
            vec![flow(Q, DSO, "Amulet", "10", FlowKind::Holding)]
        );
        assert_eq!(
            f.roots,
            vec![ExerciseFact {
                template: "Splice.ExternalPartyAmuletRules:ExternalPartyAmuletRules".into(),
                choice: "TransferFactory_Transfer".into(),
                contract_id: "00".into(),
            }]
        );
        assert_eq!(f.act_as, vec![P.to_string()]);
        assert!(
            f.unmodelled.is_empty() && f.dvp.is_empty() && f.atomic_settle.is_none(),
            "{f:?}"
        );
    }

    #[test]
    fn a_second_root_that_moves_holdings_is_reported() {
        let mut p = transfer_tx(vec![amulet("c1", P, "1.0")]);
        let second = root(
            ("Splice.AmuletRules", "AmuletRules"),
            "AmuletRules_Transfer",
            &["c"],
        );
        add_root(
            &mut p,
            "r2",
            second,
            vec![create_node("c", amulet("c9", X, "3.25"))],
        );
        let f = facts(&p);
        assert_eq!(
            f.outflows,
            vec![flow(X, DSO, "Amulet", "3.25", FlowKind::Holding)]
        );
        let choices: Vec<&str> = f.roots.iter().map(|r| r.choice.as_str()).collect();
        assert_eq!(
            choices,
            ["TransferFactory_Transfer", "AmuletRules_Transfer"]
        );
    }

    #[test]
    fn a_registry_holding_for_another_owner_is_a_holding_flow() {
        let holding = |cid: &str, owner: &str| {
            let arg = record(vec![
                ("registrar", party(REG)),
                ("owner", party(owner)),
                (
                    "instrument",
                    record(vec![
                        ("source", party(REG)),
                        ("id", text("TOK")),
                        ("scheme", text("s")),
                    ]),
                ),
                ("amount", numeric("2.0000000000")),
                ("lock", optional(None)),
            ]);
            with_cid(
                create(
                    ident("Utility.Registry.Holding.V0.Holding", "Holding"),
                    arg,
                    &[REG, owner],
                ),
                cid,
            )
        };
        let f = facts(&transfer_tx(vec![holding("h1", Q), holding("h2", P)]));
        assert_eq!(
            f.outflows,
            vec![flow(Q, REG, "TOK", "2", FlowKind::Holding)]
        );
    }

    #[test]
    fn a_cip56_instruction_gives_a_pending_transfer() {
        // the lock that funds the instruction is not reported again
        let f = facts(&transfer_tx(vec![
            locked("lock1", P, "5.6", &[DSO]),
            instruction("ti1", "lock1", P, Q, "5.5"),
            amulet("c3", P, "4.4"),
        ]));
        assert_eq!(
            f.outflows,
            vec![flow(Q, DSO, "Amulet", "5.5", FlowKind::PendingTransfer)]
        );

        let offer_arg = record(vec![
            ("operator", party(REG)),
            ("provider", party(REG)),
            ("transfer", leg(P, Q, "7.0", REG, "TOK")),
        ]);
        let offer = with_cid(
            create(
                ident("Utility.Registry.App.V0.Model.Transfer", "TransferOffer"),
                offer_arg,
                &[REG, P],
            ),
            "o1",
        );
        let f = facts(&transfer_tx(vec![
            offer,
            instruction("ti2", "elsewhere", X, P, "1.0"),
        ]));
        assert_eq!(
            f.outflows,
            vec![flow(Q, REG, "TOK", "7", FlowKind::PendingTransfer)]
        );
    }

    #[test]
    fn locks_to_other_parties_are_lock_flows() {
        let f = facts(&transfer_tx(vec![
            locked("l1", P, "2.0", &[P, X]),
            locked("l2", P, "1.5", &[DSO]),
            locked("l3", P, "1.0", &[P]),
        ]));
        assert_eq!(
            f.outflows,
            vec![
                flow(X, DSO, "Amulet", "2", FlowKind::Lock),
                flow(DSO, DSO, "Amulet", "1.5", FlowKind::Lock)
            ]
        );
        // a funding lock that someone else also holds is reported
        let f = facts(&transfer_tx(vec![
            locked("l4", P, "5.6", &[DSO, X]),
            instruction("ti", "l4", P, Q, "5.5"),
        ]));
        assert_eq!(
            f.outflows,
            vec![
                flow(Q, DSO, "Amulet", "5.5", FlowKind::PendingTransfer),
                flow(&format!("{DSO},{X}"), DSO, "Amulet", "5.6", FlowKind::Lock),
            ]
        );
    }

    #[test]
    fn an_allocation_gives_an_allocation_flow() {
        let f = facts(&transfer_tx(vec![
            locked("la", P, "3.0", &[DSO]),
            allocation("a1", "la", P, Q, "3.0"),
        ]));
        assert_eq!(
            f.outflows,
            vec![flow(Q, DSO, "Amulet", "3", FlowKind::Allocation)]
        );

        let arg = record(vec![
            (
                "allocation",
                record(vec![("transferLeg", leg(P, Q, "9.0", REG, "TOK"))]),
            ),
            ("lockedHoldingCid", contract_id("h1")),
            ("operator", party(REG)),
        ]);
        let t = ident("Utility.Registry.V0.Holding.Allocation", "DvpLegAllocation");
        let f = facts(&transfer_tx(vec![with_cid(
            create(t, arg, &[REG, P]),
            "a2",
        )]));
        assert_eq!(
            f.outflows,
            vec![flow(Q, REG, "TOK", "9", FlowKind::Allocation)]
        );
        // an allocation of a leg the party receives moves nothing of its own
        assert!(
            facts(&transfer_tx(vec![allocation("a3", "lx", Q, P, "1.0")]))
                .outflows
                .is_empty()
        );
    }

    #[test]
    fn a_withdraw_attestation_of_the_party_is_a_burn() {
        let valid = |p: &str| Value {
            sum: Some(Sum::Variant(Box::new(v2::Variant {
                variant_id: None,
                constructor: "Valid".into(),
                value: Some(Box::new(party(p))),
            }))),
        };
        let att = record(vec![
            ("amount", numeric("10.5000000000")),
            ("sourceDepositor", valid(P)),
            ("destinationRecipient", text("0xabc")),
            ("instrumentId", instrument_id(REG, "USDCx")),
        ]);
        let att = with_cid(
            create(
                ident(
                    "Utility.Bridge.V0.Attestation.Withdraw",
                    "WithdrawAttestation",
                ),
                att,
                &[REG, P],
            ),
            "w1",
        );
        let agreement = ("Utility.Bridge.V0.Agreement.User", "BridgeUserAgreement");
        let p = prepared(
            &["0"],
            vec![
                exercise_node("0", root(agreement, "BridgeUserAgreement_Burn", &["1"])),
                create_node("1", att),
            ],
            vec![],
            &[P],
        );
        let f = facts(&p);
        assert_eq!(
            f.outflows,
            vec![flow("0xabc", REG, "USDCx", "10.5", FlowKind::Burn)]
        );
        assert!(f.unmodelled.is_empty(), "{f:?}");
    }

    #[test]
    fn amounts_are_exact() {
        let f = facts(&transfer_tx(vec![
            amulet("c1", Q, "12345678901234567.0000000001"),
            amulet("c2", X, "0.0000000001"),
        ]));
        let amounts: Vec<String> = f.outflows.iter().map(|f| f.amount.to_string()).collect();
        assert_eq!(amounts, ["12345678901234567.0000000001", "0.0000000001"]);
        for bad in [
            "1e3",
            "+1.0",
            "1.",
            " 1.0",
            "-1.0",
            "99999999999999999999999999999999999999.0",
            "",
        ] {
            let reason = malformed_reason(&transfer_tx(vec![amulet("c1", Q, bad)]));
            assert!(reason.contains("initialAmount"), "{bad:?}: {reason}");
        }
    }

    #[test]
    fn an_unknown_template_lands_in_unmodelled() {
        let thing = with_cid(
            create(ident("Some.Module", "Thing"), record(vec![]), &[P]),
            "t1",
        );
        let coupon = with_cid(
            create(
                ident("Splice.Amulet", "AppRewardCoupon"),
                record(vec![]),
                &[DSO],
            ),
            "t2",
        );
        let mut p = transfer_tx(vec![thing, coupon]);
        add_root(&mut p, "r2", root(("Other", "T"), "Go", &[]), vec![]);
        let f = facts(&p);
        assert_eq!(f.unmodelled, ["Other:T", "Some.Module:Thing"]);
        assert!(f.outflows.is_empty());
        assert_eq!(f.roots.len(), 2);
    }

    #[test]
    fn contracts_consumed_in_the_same_transaction_report_only_what_replaces_them() {
        // an instruction accepted in the same transaction: only the receiver's holding moves
        let mut p = transfer_tx(vec![
            locked("L", P, "5.0", &[DSO]),
            instruction("I", "L", P, Q, "5.0"),
        ]);
        let mut accept = root(
            (
                "Splice.AmuletTransferInstruction",
                "AmuletTransferInstruction",
            ),
            "TransferInstruction_Accept",
            &["u", "c"],
        );
        accept.contract_id = "I".into();
        accept.consuming = true;
        let mut unlock = root(
            ("Splice.Amulet", "LockedAmulet"),
            "LockedAmulet_Unlock",
            &[],
        );
        unlock.contract_id = "L".into();
        unlock.consuming = true;
        add_root(
            &mut p,
            "r2",
            accept,
            vec![
                exercise_node("u", unlock),
                create_node("c", amulet("A", Q, "5.0")),
            ],
        );
        assert_eq!(
            facts(&p).outflows,
            vec![flow(Q, DSO, "Amulet", "5", FlowKind::Holding)]
        );
    }

    #[test]
    fn rolled_back_nodes_move_nothing() {
        let p = prepared(
            &["0"],
            vec![
                exercise_node("0", root(RULES, "TransferFactory_Transfer", &["rb"])),
                rollback_node("rb", ids(&["c"])),
                create_node("c", amulet("c1", X, "1.0")),
            ],
            vec![],
            &[P],
        );
        let f = facts(&p);
        assert!(f.outflows.is_empty(), "{f:?}");
    }

    fn iq(id: &str, admin: &str, amount: &str) -> Value {
        record(vec![
            (
                "instrument",
                record(vec![("id", text(id)), ("admin", party(admin))]),
            ),
            ("amount", numeric(amount)),
        ])
    }

    fn terms(deliveries: Vec<Value>) -> Value {
        record(vec![
            ("id", text("prop")),
            ("deliveries", list(deliveries)),
            ("payments", list(vec![iq("Amulet", DSO, "2.5")])),
        ])
    }

    fn proposal(deliveries: Vec<Value>) -> Create {
        let arg = record(vec![
            ("operator", party(REG)),
            ("proposer", party(P)),
            ("proposerIsBuyer", boolean(false)),
            ("counterparty", party(Q)),
            ("terms", terms(deliveries)),
        ]);
        with_cid(create(ident(DVP_MODULE, "DvpProposal"), arg, &[P]), "dp1")
    }

    #[test]
    fn dvp_terms_come_from_created_and_input_contracts() {
        let expected = DvpTerms {
            proposal_id: Some("prop".into()),
            buyer: Q.into(),
            seller: P.into(),
            delivery: Quantity {
                instrument_admin: REG.into(),
                instrument_id: "D".into(),
                amount: dec("1"),
            },
            payment: Quantity {
                instrument_admin: DSO.into(),
                instrument_id: "Amulet".into(),
                amount: dec("2.5"),
            },
        };
        let deliveries = || vec![iq("D", REG, "1.0")];
        let propose = prepared(
            &["0"],
            vec![
                exercise_node("0", root(USER_SERVICE, "UserService_ProposeDvp", &["1"])),
                create_node("1", proposal(deliveries())),
            ],
            vec![],
            &[P],
        );
        let f = facts(&propose);
        assert_eq!(f.dvp, vec![expected.clone()]);
        assert!(f.unmodelled.is_empty() && f.outflows.is_empty(), "{f:?}");

        // the accepted proposal is an input contract; the created Dvp repeats its terms
        let dvp_arg = record(vec![
            ("operator", party(REG)),
            ("buyer", party(Q)),
            ("seller", party(P)),
            ("terms", terms(deliveries())),
        ]);
        let dvp = with_cid(create(ident(DVP_MODULE, "Dvp"), dvp_arg, &[P, Q]), "d1");
        let accept = prepared(
            &["0"],
            vec![
                exercise_node(
                    "0",
                    root(USER_SERVICE, "UserService_AcceptDvpProposal", &["1"]),
                ),
                create_node("1", dvp),
            ],
            vec![],
            &[P],
        );
        assert_eq!(
            facts(&with_input_contract(accept, proposal(deliveries()))).dvp,
            vec![expected]
        );

        let two = vec![iq("D", REG, "1.0"), iq("D", REG, "1.0")];
        let p = prepared(
            &["0"],
            vec![
                exercise_node("0", root(USER_SERVICE, "UserService_ProposeDvp", &["1"])),
                create_node("1", proposal(two)),
            ],
            vec![],
            &[P],
        );
        assert!(malformed_reason(&p).contains("deliveries"));
    }

    fn settle(venue_cid: &str) -> Exercise {
        let side = Value {
            sum: Some(Sum::Enum(v2::Enum {
                enum_id: None,
                constructor: "Sell".into(),
            })),
        };
        let quote = record(vec![
            ("quoteId", text("q1")),
            ("ticketId", text("")),
            ("user", party(P)),
            ("side", side),
            ("baseAmount", numeric("4.0")),
            ("quoteAmount", numeric("8.0")),
        ]);
        let mut ex = root(("AtomicDVP", "AtomicDVP"), "AtomicDVP_Settle", &[]);
        ex.contract_id = venue_cid.into();
        ex.chosen_value = Some(record(vec![
            ("quote", quote),
            ("lpInputHoldingCids", list(vec![contract_id("h-lp")])),
            ("userInputHoldingCids", list(vec![contract_id("h-user")])),
        ]));
        ex
    }

    #[test]
    fn an_atomic_settle_gives_settle_facts() {
        let venue_arg = record(vec![
            ("provider", party(REG)),
            ("lp", party(Q)),
            ("pairName", text("TOK/CC")),
            ("baseInstrumentId", instrument_id(REG, "TOK")),
            ("quoteInstrumentId", instrument_id(DSO, "Amulet")),
            ("quotePublicKey", text("k")),
        ]);
        let venue = with_cid(
            create(ident("AtomicDVP", "AtomicDVP"), venue_arg, &[Q, REG]),
            "v1",
        );
        let mut ex = settle("v1");
        ex.children = ids(&["1", "2"]);
        let p = prepared(
            &["0"],
            vec![
                exercise_node("0", ex),
                create_node("1", amulet("c1", Q, "4.0")),
                create_node("2", amulet("c2", P, "8.0")),
            ],
            vec![],
            &[P],
        );
        let f = facts(&with_input_contract(p.clone(), venue));
        assert_eq!(
            f.outflows,
            vec![flow(Q, DSO, "Amulet", "4", FlowKind::Holding)]
        );
        let s = f.atomic_settle.unwrap();
        assert_eq!(
            (s.quote_id.as_str(), s.user.as_str(), s.side),
            ("q1", P, QuoteSide::Sell)
        );
        assert_eq!((s.base_amount, s.quote_amount), (dec("4"), dec("8")));
        assert_eq!(
            (s.lp_input_holding_cids, s.user_input_holding_cids),
            (vec!["h-lp".to_string()], vec!["h-user".to_string()])
        );
        let venue = s.venue.unwrap();
        assert_eq!((venue.lp.as_str(), venue.pair_name.as_str()), (Q, "TOK/CC"));
        assert_eq!(
            venue.base,
            Instrument {
                admin: REG.into(),
                id: "TOK".into()
            }
        );
        // without the venue input contract the venue terms are absent
        assert!(facts(&p).atomic_settle.unwrap().venue.is_none());

        let mut twice = p;
        add_root(&mut twice, "r2", settle("v1"), vec![]);
        assert!(malformed_reason(&twice).contains("more than one settle"));
    }

    #[test]
    fn malformed_records_are_errors() {
        let bad_arg = |arg: Value| {
            let mut c = amulet("c1", Q, "1.0");
            c.argument = Some(arg);
            transfer_tx(vec![c])
        };
        let wrong_owner = record(vec![
            ("dso", party(DSO)),
            ("owner", text(Q)),
            ("amount", record(vec![])),
        ]);
        assert!(malformed_reason(&bad_arg(wrong_owner)).contains("owner is not a party"));
        let no_amount = record(vec![("dso", party(DSO)), ("owner", party(Q))]);
        assert!(malformed_reason(&bad_arg(no_amount)).contains("missing field amount"));
        assert!(malformed_reason(&bad_arg(text("x"))).contains("no record argument"));
        let mut c = instruction("ti", "l", P, Q, "1.0");
        c.argument = Some(record(vec![
            ("lockedAmulet", contract_id("l")),
            ("transfer", record(vec![("sender", party(P))])),
        ]));
        assert!(malformed_reason(&transfer_tx(vec![c])).contains("missing field"));
        let mut c = locked("l1", P, "1.0", &[X]);
        c.argument = Some(record(vec![
            ("amulet", amulet_value(P, "1.0")),
            ("lock", record(vec![("holders", list(vec![text(X)]))])),
        ]));
        assert!(malformed_reason(&transfer_tx(vec![c])).contains("lock holder"));
        let mut c = amulet("c1", Q, "1.0");
        c.template_id = None;
        assert!(malformed_reason(&transfer_tx(vec![c])).contains("no template id"));
    }

    #[test]
    fn malformed_transactions_are_errors() {
        let ok = transfer_tx(vec![amulet("c1", Q, "1.0")]);
        assert!(extract_facts(&encode(&ok), P).is_ok());
        let tx = |f: &dyn Fn(&mut DamlTransaction)| {
            let mut p = ok.clone();
            f(p.transaction.as_mut().unwrap());
            malformed_reason(&p)
        };
        assert!(tx(&|t| t.roots.clear()).contains("no root nodes"));
        assert!(tx(&|t| t.roots = ids(&["nope"])).contains("node nope is missing"));
        assert!(tx(&|t| t.roots.push("0".into())).contains("reached twice"));
        assert!(
            tx(&|t| {
                let first = t.nodes.first().unwrap().clone();
                t.nodes.push(first);
            })
            .contains("used twice")
        );
        assert!(
            tx(&|t| t.nodes.push(create_node("orphan", amulet("c7", X, "1.0"))))
                .contains("no root reaches")
        );
        assert!(
            tx(&|t| t.nodes.push(Node {
                node_id: "x".into(),
                versioned_node: None
            }))
            .contains("no content")
        );
        assert!(
            malformed_reason(&transfer_tx(vec![
                amulet("c1", Q, "1.0"),
                amulet("c1", X, "1.0")
            ]))
            .contains("created twice")
        );

        let mut p = ok.clone();
        p.transaction = None;
        assert_eq!(malformed_reason(&p), "missing transaction");
        let mut p = ok;
        p.metadata = None;
        assert_eq!(malformed_reason(&p), "missing metadata");
        assert!(matches!(
            extract_facts(&[0xff], P),
            Err(FactsError::Decode(_))
        ));
    }

    #[test]
    fn nesting_is_bounded() {
        let chain = |len: usize| {
            let nodes = (0..len)
                .map(|i| {
                    let mut ex = root(RULES, "C", &[]);
                    if i + 1 < len {
                        ex.children = vec![(i + 1).to_string()];
                    }
                    exercise_node(&i.to_string(), ex)
                })
                .collect();
            prepared(&["0"], nodes, vec![], &[P])
        };
        assert!(extract_facts(&encode(&chain(MAX_NODE_DEPTH)), P).is_ok());
        assert!(malformed_reason(&chain(MAX_NODE_DEPTH + 1)).contains("too deep"));
        assert!(malformed_reason(&chain(20_000)).contains("too deep"));
    }

    #[test]
    fn mangled_bytes_never_panic() {
        let mut p = transfer_tx(vec![
            locked("lock1", P, "5.6", &[DSO]),
            instruction("ti1", "lock1", P, Q, "5.5"),
            amulet("c3", Q, "4.4"),
        ]);
        add_root(&mut p, "r2", settle("v1"), vec![]);
        let bytes = encode(&p);
        assert!(extract_facts(&bytes, P).is_ok());
        for n in 0..bytes.len() {
            let _ = extract_facts(&bytes[..n], P);
        }
        for i in 0..bytes.len() {
            for mask in [0x01u8, 0x80, 0xff] {
                let mut b = bytes.clone();
                b[i] ^= mask;
                let _ = extract_facts(&b, P);
            }
        }
    }
}
