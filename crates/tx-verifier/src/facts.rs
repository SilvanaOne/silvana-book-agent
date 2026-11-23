//! Whole-transaction facts read from prepared transaction bytes: root exercises,
//! value flows, DvP terms, settle terms, package ids and templates outside the model.

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
use crate::text::short;
use v2::interactive::metadata::input_contract::Contract as InputContract;
use v2::interactive::transaction::v1::node::NodeType;
use v2::interactive::transaction::v1::{Create, Exercise};
use v2::interactive::{DamlTransaction, Metadata, PreparedTransaction};
use v2::{Identifier, Record, Value, value::Sum};

/// Deepest node nesting followed (roots are level 1).
const MAX_NODE_DEPTH: usize = 256;
const AMULET_ID: &str = "Amulet";
/// The choice of an atomic settle on a venue.
pub const SETTLE_CHOICE: &str = "AtomicDVP_Settle";
/// The venue template, as `module:entity`.
pub const VENUE_TEMPLATE: &str = "AtomicDVP:AtomicDVP";
const DVP_MODULE: &str = "Utility.Settlement.App.V1.Model.Dvp";

/// The facts of one prepared transaction, as seen by one party.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TxFacts {
    /// The submitting parties named in the metadata.
    pub act_as: Vec<String>,
    /// The command id named in the metadata.
    pub command_id: String,
    /// Every root exercise, in order.
    pub roots: Vec<ExerciseFact>,
    /// The number of root nodes of every type; `roots` lists only the exercises.
    pub root_nodes: usize,
    /// Holdings created for other parties, and the party's pending transfers,
    /// allocations, locks and burns, in node order.
    pub outflows: Vec<ValueFlow>,
    /// Amulets and registry holdings created for the party and not consumed in the
    /// same transaction, in node order.
    pub inflows: Vec<HoldingFact>,
    /// The party's amulets, locked amulets and registry holdings that the transaction
    /// consumes, as its input contracts state them, in node order.
    pub consumed: Vec<HoldingFact>,
    /// Contracts consumed outside rolled-back nodes that are neither created in the
    /// transaction nor listed among its input contracts, sorted and without repeats.
    pub unlisted_inputs: Vec<String>,
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

/// A holding of the party.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldingFact {
    pub contract_id: String,
    pub instrument_admin: String,
    pub instrument_id: String,
    pub amount: Decimal,
    /// A locked amulet, or a registry holding with its `lock` set.
    pub locked: bool,
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
    /// The quote's expiry, in microseconds since the epoch.
    pub valid_until_micros: i64,
    pub lp_input_holding_cids: Vec<String>,
    pub user_input_holding_cids: Vec<String>,
    /// The fee transfers the settle submits, in order; empty when it submits none.
    pub fees: Vec<FeeLeg>,
    /// From the venue input contract, when the transaction carries it.
    pub venue: Option<VenueTerms>,
}

/// One fee transfer of an atomic settle, sent by the quote's user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeLeg {
    pub receiver: String,
    /// The fee instrument.
    pub admin: String,
    pub id: String,
    pub amount: Decimal,
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

/// The package ids a prepared transaction names, each sorted and without repeats.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageIds {
    /// The template package of every Create, Exercise and Fetch node and the interface
    /// package of every Exercise and Fetch, rolled-back nodes included.
    pub enforced: Vec<String>,
    /// The packages the input contracts were created with.
    pub inputs: Vec<String>,
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
        command_id: metadata
            .submitter_info
            .as_ref()
            .map(|s| s.command_id.clone())
            .unwrap_or_default(),
        root_nodes: tx.roots.len(),
        ..TxFacts::default()
    };
    let mut unmodelled = BTreeSet::new();
    let mut dvp_seen = HashSet::new();
    let mut created = HashSet::new();
    let mut created_live = HashSet::new();
    let mut consumed = HashSet::new();
    let mut consumed_in_order = Vec::new();
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
                if !visit.rolled_back {
                    created_live.insert(c.contract_id.as_str());
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
                if e.consuming && consumed.insert(e.contract_id.as_str()) {
                    consumed_in_order.push(e.contract_id.as_str());
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

    // A contract listed twice among the inputs has no single amount to read (`None`).
    let mut inputs: HashMap<&str, Option<&Create>> = HashMap::new();
    for input in &metadata.input_contracts {
        let Some(InputContract::V1(c)) = &input.contract else {
            continue;
        };
        inputs
            .entry(c.contract_id.as_str())
            .and_modify(|seen| *seen = None)
            .or_insert(Some(c));
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
    let unlisted: BTreeSet<&str> = consumed_in_order
        .iter()
        .copied()
        .filter(|cid| !created_live.contains(cid) && !inputs.contains_key(cid))
        .collect();
    facts.unlisted_inputs = unlisted.into_iter().map(str::to_string).collect();

    // A contract consumed in the same transaction moves value only through what
    // its consuming choice creates, which is read on its own.
    let mut backing = HashSet::new();
    let mut locks = Vec::new();
    for &(model, c) in &creates {
        let arg = argument(c)?;
        let transient = consumed.contains(c.contract_id.as_str());
        let flow = match model {
            Model::Amulet | Model::RegistryHolding if !transient => {
                let (owner, held) = holding(&c.contract_id, arg, model)?;
                if owner == party {
                    facts.inflows.push(held);
                    None
                } else {
                    Some(ValueFlow {
                        to: owner.to_string(),
                        instrument_admin: held.instrument_admin,
                        instrument_id: held.instrument_id,
                        amount: held.amount,
                        kind: FlowKind::Holding,
                    })
                }
            }
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
    for cid in consumed_in_order {
        let c = match inputs.get(cid) {
            None => continue,
            Some(None) => {
                return Err(malformed(format!(
                    "consumed contract {cid} is an input contract twice"
                )))
            }
            Some(Some(c)) => *c,
        };
        let (owner, held) = match c.template_id.as_ref().and_then(model_of) {
            Some(model @ (Model::Amulet | Model::RegistryHolding)) => {
                holding(cid, argument(c)?, model)?
            }
            Some(Model::LockedAmulet) => {
                let amulet = record_field(argument(c)?, "amulet")?;
                let (owner, mut held) = holding(cid, amulet, Model::Amulet)?;
                held.locked = true;
                (owner, held)
            }
            _ => continue,
        };
        if owner == party {
            facts.consumed.push(held);
        }
    }
    facts.unmodelled = unmodelled.into_iter().collect();
    Ok(facts)
}

/// Reads the package ids of every node and input contract of a prepared transaction;
/// a package id that is not 64 lowercase hex characters is an error.
pub fn package_ids(prepared_transaction_bytes: &[u8]) -> Result<PackageIds, FactsError> {
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
    let mut enforced = BTreeSet::new();
    for visit in walk(tx)? {
        let (template_id, interface_id) = match visit.node {
            NodeType::Create(c) => (c.template_id.as_ref(), None),
            NodeType::Exercise(e) => (e.template_id.as_ref(), e.interface_id.as_ref()),
            NodeType::Fetch(f) => (f.template_id.as_ref(), f.interface_id.as_ref()),
            NodeType::Rollback(_) => continue,
        };
        let id = template_id.ok_or_else(|| malformed("a node has no template id"))?;
        enforced.insert(package_id(id)?);
        if let Some(id) = interface_id {
            enforced.insert(package_id(id)?);
        }
    }
    let mut inputs = BTreeSet::new();
    for input in &metadata.input_contracts {
        let Some(InputContract::V1(c)) = &input.contract else {
            continue;
        };
        let id = c.template_id.as_ref().ok_or_else(|| {
            malformed(format!("input contract {} has no template id", c.contract_id))
        })?;
        inputs.insert(package_id(id)?);
    }
    Ok(PackageIds {
        enforced: enforced.into_iter().collect(),
        inputs: inputs.into_iter().collect(),
    })
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
    (
        "Splice.ExternalPartyConfigState",
        "ExternalPartyConfigState",
        Model::Neutral,
    ),
    ("Splice.AmuletEventLog", "AmuletEventLog", Model::Neutral),
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
        "Utility.Registry.V0.Holding.Transfer",
        "ExecutedTransfer",
        Model::Neutral,
    ),
    (
        "Utility.Registry.V0.Holding.Transfer",
        "RejectedTransfer",
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
        "Utility.Credential.V0.Credential",
        "Credential",
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

/// The owner of an amulet (`model` Amulet) or a registry holding, and the holding.
fn holding<'a>(
    cid: &str,
    arg: &'a Record,
    model: Model,
) -> Result<(&'a str, HoldingFact), FactsError> {
    let owner = party_field(arg, "owner")?;
    let (admin, id, amount, locked) = match model {
        Model::Amulet => (
            party_field(arg, "dso")?,
            AMULET_ID,
            amount_field(record_field(arg, "amount")?, "initialAmount")?,
            false,
        ),
        _ => {
            let instrument = record_field(arg, "instrument")?;
            (
                party_field(instrument, "source")?,
                text_field(instrument, "id")?,
                amount_field(arg, "amount")?,
                has_lock(arg)?,
            )
        }
    };
    Ok((
        owner,
        HoldingFact {
            contract_id: cid.to_string(),
            instrument_admin: admin.to_string(),
            instrument_id: id.to_string(),
            amount,
            locked,
        },
    ))
}

/// Whether a registry holding's `lock : Optional Lock` is set; a trailing `None` may be
/// left out.
fn has_lock(arg: &Record) -> Result<bool, FactsError> {
    match get_record_field(arg, "lock").map(|v| v.sum.as_ref()) {
        None => Ok(false),
        Some(Some(Sum::Optional(o))) => Ok(o.value.is_some()),
        Some(_) => Err(malformed("lock is not an Optional")),
    }
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
        valid_until_micros: int_field(quote, "validUntilMicros")?,
        lp_input_holding_cids: cid_list(chosen, "lpInputHoldingCids")?,
        user_input_holding_cids: cid_list(chosen, "userInputHoldingCids")?,
        fees: fee_legs(chosen)?,
        venue: venue_terms(metadata, &e.contract_id)?,
    })
}

/// The settle's `fees : Optional [FeeTransfer]`; the field may be left out when `None`.
fn fee_legs(chosen: &Record) -> Result<Vec<FeeLeg>, FactsError> {
    let fees = match get_record_field(chosen, "fees").map(|v| v.sum.as_ref()) {
        None => return Ok(Vec::new()),
        Some(Some(Sum::Optional(o))) => match o.value.as_deref() {
            None => return Ok(Vec::new()),
            Some(v) => as_list(v).ok_or_else(|| malformed("fees is not a list"))?,
        },
        Some(_) => return Err(malformed("fees is not an Optional")),
    };
    fees.iter()
        .map(|fee| {
            let fee = as_record(fee).ok_or_else(|| malformed("fees holds a non-record"))?;
            let instrument = record_field(fee, "instrumentId")?;
            Ok(FeeLeg {
                receiver: party_field(fee, "receiver")?.to_string(),
                admin: party_field(instrument, "admin")?.to_string(),
                id: text_field(instrument, "id")?.to_string(),
                amount: amount_field(fee, "amount")?,
            })
        })
        .collect()
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

fn int_field(r: &Record, label: &str) -> Result<i64, FactsError> {
    match field(r, label)?.sum.as_ref() {
        Some(Sum::Int64(n)) => Ok(*n),
        _ => Err(malformed(format!("{label} is not an Int64"))),
    }
}

/// The package id of `id`, which must be 64 lowercase hex characters.
fn package_id(id: &Identifier) -> Result<String, FactsError> {
    let pkg = id.package_id.as_str();
    if pkg.len() == 64 && pkg.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Ok(pkg.to_string());
    }
    Err(malformed(format!(
        "the package id {:?} of {}:{} is not 64 lowercase hex",
        short(pkg, 80),
        id.module_name,
        id.entity_name
    )))
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
    const TRAFFIC: &str = "traffic::1220cc";
    const CONFIG: (&str, &str) = ("Splice.ExternalPartyConfigState", "ExternalPartyConfigState");
    const AMULET_T: (&str, &str) = ("Splice.Amulet", "Amulet");
    const HOLDING_T: (&str, &str) = ("Utility.Registry.Holding.V0.Holding", "Holding");
    const VALID_UNTIL: i64 = 1_795_378_950_000_000;

    fn dec(s: &str) -> Decimal {
        parse_exact(s).unwrap()
    }

    fn int64(n: i64) -> Value {
        Value {
            sum: Some(Sum::Int64(n)),
        }
    }

    fn held(cid: &str, admin: &str, id: &str, amount: &str) -> HoldingFact {
        HoldingFact {
            contract_id: cid.into(),
            instrument_admin: admin.into(),
            instrument_id: id.into(),
            amount: dec(amount),
            locked: false,
        }
    }

    /// A consuming exercise by P of `t` on `cid`.
    fn spend(cid: &str, t: (&str, &str)) -> Exercise {
        let mut ex = exercise("Archive", &[P], vec![]);
        ex.template_id = Some(ident(t.0, t.1));
        ex.contract_id = cid.into();
        ex
    }

    fn reg_holding(cid: &str, owner: &str, amount: &str) -> Create {
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
            ("amount", numeric(amount)),
        ]);
        with_cid(
            create(ident(HOLDING_T.0, HOLDING_T.1), arg, &[REG, owner]),
            cid,
        )
    }

    /// The config state a CC transfer context names.
    fn config_state(cid: &str) -> Create {
        let arg = record(vec![
            ("dso", party(DSO)),
            ("amuletPrice", numeric("0.15")),
            ("targetArchiveAfter", record(vec![])),
        ]);
        with_cid(create(ident(CONFIG.0, CONFIG.1), arg, &[DSO]), cid)
    }

    /// The event-log choice a CC transfer exercises on the config state.
    fn holdings_change(cid: &str) -> Exercise {
        let mut ex = exercise("EventLog_HoldingsChange", &[DSO], vec![]);
        ex.template_id = Some(ident(CONFIG.0, CONFIG.1));
        ex.interface_id = Some(ident("Splice.Api.Token.TransferEventsV2", "EventLog"));
        ex.contract_id = cid.into();
        ex.consuming = false;
        ex
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
            ("createdAtMicros", int64(VALID_UNTIL - 60_000_000)),
            ("validUntilMicros", int64(VALID_UNTIL)),
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

    /// A traffic prepayment: P's input amulet pays the traffic party with change back to
    /// P, and the transfer fetches the config state and logs the change on it.
    fn prepay_tx() -> PreparedTransaction {
        let marker = with_cid(
            create(
                ident("Splice.Amulet", "FeaturedAppActivityMarker"),
                record(vec![]),
                &[DSO],
            ),
            "m1",
        );
        let p = prepared(
            &["0"],
            vec![
                exercise_node(
                    "0",
                    root(
                        RULES,
                        "TransferFactory_Transfer",
                        &["f", "s", "l", "t", "c", "m"],
                    ),
                ),
                fetch_node("f", fetch(ident(CONFIG.0, CONFIG.1), "cs1", &[P])),
                exercise_node("s", spend("in1", AMULET_T)),
                exercise_node("l", holdings_change("cs1")),
                create_node("t", amulet("c1", TRAFFIC, "500.0000000000")),
                create_node("c", amulet("c2", P, "9.5000000000")),
                create_node("m", marker),
            ],
            vec![],
            &[P],
        );
        with_input_contract(
            with_input_contract(p, config_state("cs1")),
            amulet("in1", P, "509.5000000000"),
        )
    }

    #[test]
    fn a_traffic_prepayment_moves_only_its_payment() {
        let f = facts(&prepay_tx());
        assert!(f.unmodelled.is_empty(), "{f:?}");
        assert_eq!(
            f.outflows,
            vec![flow(TRAFFIC, DSO, "Amulet", "500", FlowKind::Holding)]
        );
        assert_eq!(f.inflows, vec![held("c2", DSO, "Amulet", "9.5")]);
        assert_eq!(f.consumed, vec![held("in1", DSO, "Amulet", "509.5")]);
    }

    #[test]
    fn a_created_or_archived_config_state_moves_nothing() {
        // cs1 is an input contract the DSO archives; cs2 is created and archived here
        let archive = |cid: &str| {
            let mut ex = spend(cid, CONFIG);
            ex.acting_parties = vec![DSO.into()];
            ex
        };
        let mut p = transfer_tx(vec![config_state("cs2"), amulet("c1", Q, "1.0")]);
        add_root(&mut p, "r2", archive("cs1"), vec![]);
        add_root(&mut p, "r3", archive("cs2"), vec![]);
        let f = facts(&with_input_contract(p, config_state("cs1")));
        assert!(f.unmodelled.is_empty(), "{f:?}");
        assert_eq!(
            f.outflows,
            vec![flow(Q, DSO, "Amulet", "1", FlowKind::Holding)]
        );
        assert!(f.inflows.is_empty() && f.consumed.is_empty(), "{f:?}");
        assert_eq!(f.roots.len(), 3);
    }

    #[test]
    fn the_config_state_inside_cc_transfers_moves_nothing() {
        let transfer_nodes = |id: &str, receiver: &str, amount: &str| {
            let child = |n: &str| format!("{id}{n}");
            let (l, t) = (child("l"), child("t"));
            vec![
                exercise_node(
                    id,
                    root(RULES, "TransferFactory_Transfer", &[l.as_str(), t.as_str()]),
                ),
                exercise_node(&l, holdings_change("cs1")),
                create_node(&t, amulet(&format!("{id}c"), receiver, amount)),
            ]
        };
        let direct = prepared(&["a"], transfer_nodes("a", Q, "10.0"), vec![], &[P]);
        let f = facts(&with_input_contract(direct, config_state("cs1")));
        assert!(f.unmodelled.is_empty(), "{f:?}");
        assert_eq!(
            f.outflows,
            vec![flow(Q, DSO, "Amulet", "10", FlowKind::Holding)]
        );

        // two transfers inside one multicall
        let mut nodes = vec![exercise_node(
            "mc",
            root(("MultiCall", "MultiCall"), "MultiCall_Execute", &["a", "b"]),
        )];
        nodes.extend(transfer_nodes("a", Q, "10.0"));
        nodes.extend(transfer_nodes("b", X, "0.5"));
        let multicall = prepared(&["mc"], nodes, vec![], &[P]);
        let f = facts(&with_input_contract(multicall, config_state("cs1")));
        assert!(f.unmodelled.is_empty(), "{f:?}");
        assert_eq!(
            f.outflows,
            vec![
                flow(Q, DSO, "Amulet", "10", FlowKind::Holding),
                flow(X, DSO, "Amulet", "0.5", FlowKind::Holding)
            ]
        );
        assert_eq!(f.roots.len(), 1);
    }

    #[test]
    fn other_splice_templates_stay_unmodelled() {
        // only the exact modules and entities of the config state and event log are modelled
        let mut temp_log = holdings_change("el1");
        temp_log.template_id = Some(ident("Splice.AmuletEventLog", "AmuletEventLogV2"));
        let lookalike = with_cid(
            create(
                ident("Splice.ExternalPartyAmuletRules", "ExternalPartyConfigState"),
                record(vec![("dso", party(DSO))]),
                &[DSO],
            ),
            "x1",
        );
        let other_entity = with_cid(
            create(
                ident(CONFIG.0, "ExternalPartyConfigStateV2"),
                record(vec![]),
                &[DSO],
            ),
            "x2",
        );
        let p = prepared(
            &["0"],
            vec![
                exercise_node(
                    "0",
                    root(RULES, "TransferFactory_Transfer", &["l", "x", "y", "t"]),
                ),
                exercise_node("l", temp_log),
                create_node("x", lookalike),
                create_node("y", other_entity),
                create_node("t", amulet("c1", Q, "1.0")),
            ],
            vec![],
            &[P],
        );
        let f = facts(&p);
        assert_eq!(
            f.unmodelled,
            [
                "Splice.AmuletEventLog:AmuletEventLogV2",
                "Splice.ExternalPartyAmuletRules:ExternalPartyConfigState",
                "Splice.ExternalPartyConfigState:ExternalPartyConfigStateV2",
            ]
        );
    }

    #[test]
    fn the_temporary_event_log_of_an_unlock_moves_nothing() {
        // an unlock creates, uses and archives a DSO event log around the unlocked amulet
        const LOCKED_T: (&str, &str) = ("Splice.Amulet", "LockedAmulet");
        const LOG_T: (&str, &str) = ("Splice.AmuletEventLog", "AmuletEventLog");
        let mut unlock = spend("k1", LOCKED_T);
        unlock.choice_id = "LockedAmulet_UnlockV2".into();
        unlock.children = ids(&["u", "lc", "le", "la"]);
        let log = with_cid(
            create(ident(LOG_T.0, LOG_T.1), record(vec![("dso", party(DSO))]), &[DSO]),
            "el1",
        );
        let mut used = holdings_change("el1");
        used.template_id = Some(ident(LOG_T.0, LOG_T.1));
        let mut archived = exercise("Archive", &[DSO], vec![]);
        archived.template_id = Some(ident(LOG_T.0, LOG_T.1));
        archived.contract_id = "el1".into();
        let p = prepared(
            &["0"],
            vec![
                exercise_node("0", unlock),
                create_node("u", amulet("c1", P, "5.0")),
                create_node("lc", log),
                exercise_node("le", used),
                exercise_node("la", archived),
            ],
            vec![],
            &[P],
        );
        let f = facts(&with_input_contract(p, locked("k1", P, "5.0", &[DSO])));
        assert!(f.unmodelled.is_empty(), "{f:?}");
        assert!(f.outflows.is_empty(), "{f:?}");
        assert!(f.unlisted_inputs.is_empty(), "{f:?}");
        assert_eq!(f.inflows, vec![held("c1", DSO, "Amulet", "5")]);
        assert_eq!(
            f.consumed,
            vec![HoldingFact {
                locked: true,
                ..held("k1", DSO, "Amulet", "5")
            }]
        );
    }

    #[test]
    fn the_command_id_comes_from_the_submitter_info() {
        let mut p = transfer_tx(vec![amulet("c1", Q, "1.0")]);
        assert_eq!(facts(&p).command_id, "cmd");
        let info = p
            .metadata
            .as_mut()
            .unwrap()
            .submitter_info
            .as_mut()
            .unwrap();
        info.command_id = "ctl-01a204eb".into();
        assert_eq!(facts(&p).command_id, "ctl-01a204eb");
        p.metadata.as_mut().unwrap().submitter_info = None;
        let f = facts(&p);
        assert!(f.command_id.is_empty() && f.act_as.is_empty(), "{f:?}");
    }

    #[test]
    fn holdings_created_for_the_party_are_inflows() {
        let mut p = transfer_tx(vec![
            amulet("c1", Q, "10.0"),
            amulet("c2", P, "89.5"),
            reg_holding("h1", P, "2.0"),
            reg_holding("h2", X, "3.0"),
            amulet("t1", P, "1.0"),
        ]);
        // t1 is consumed in the same transaction; a rolled-back holding moves nothing
        add_root(&mut p, "r2", spend("t1", AMULET_T), vec![]);
        add_root(
            &mut p,
            "r3",
            root(RULES, "C", &["rb"]),
            vec![
                rollback_node("rb", ids(&["x"])),
                create_node("x", amulet("c9", P, "7.0")),
            ],
        );
        let f = facts(&p);
        assert_eq!(
            f.inflows,
            vec![
                held("c2", DSO, "Amulet", "89.5"),
                held("h1", REG, "TOK", "2")
            ]
        );
        assert_eq!(
            f.outflows,
            vec![
                flow(Q, DSO, "Amulet", "10", FlowKind::Holding),
                flow(X, REG, "TOK", "3", FlowKind::Holding)
            ]
        );
        // the party's own holdings are read in full
        let reason = malformed_reason(&transfer_tx(vec![amulet("c1", P, "-1.0")]));
        assert!(reason.contains("initialAmount"), "{reason}");
        let reason = malformed_reason(&transfer_tx(vec![reg_holding("h1", P, "x")]));
        assert!(reason.contains("amount"), "{reason}");
    }

    #[test]
    fn the_partys_consumed_input_holdings_carry_their_amounts() {
        let p = prepared(
            &["0"],
            vec![
                exercise_node(
                    "0",
                    root(
                        RULES,
                        "TransferFactory_Transfer",
                        &["a", "b", "f", "q", "n", "rb", "c"],
                    ),
                ),
                exercise_node("a", spend("in1", AMULET_T)),
                exercise_node("b", spend("in2", HOLDING_T)),
                fetch_node("f", fetch(ident(AMULET_T.0, AMULET_T.1), "in3", &[P])),
                exercise_node("q", spend("in4", AMULET_T)),
                exercise_node("n", spend("in6", AMULET_T)),
                rollback_node("rb", ids(&["x"])),
                exercise_node("x", spend("in5", AMULET_T)),
                create_node("c", amulet("c1", Q, "3.0")),
            ],
            vec![],
            &[P],
        );
        // in3 is only fetched, in4 is Q's, in5 is spent in a rolled-back node, in6 is no input
        let p = [
            amulet("in1", P, "1.5"),
            reg_holding("in2", P, "2.0"),
            amulet("in3", P, "4.0"),
            amulet("in4", Q, "8.0"),
            amulet("in5", P, "16.0"),
        ]
        .into_iter()
        .fold(p, with_input_contract);
        let f = facts(&p);
        assert_eq!(
            f.consumed,
            vec![
                held("in1", DSO, "Amulet", "1.5"),
                held("in2", REG, "TOK", "2")
            ]
        );
        assert_eq!(
            f.outflows,
            vec![flow(Q, DSO, "Amulet", "3", FlowKind::Holding)]
        );
        assert!(f.inflows.is_empty(), "{f:?}");
        assert_eq!(f.unlisted_inputs, vec!["in6"]);

        // a consumed contract listed twice among the inputs has no single amount
        let twice = with_input_contract(p.clone(), amulet("in1", P, "100.0"));
        assert!(malformed_reason(&twice).contains("input contract twice"));
        // a duplicate that is not consumed is not read
        let twice = with_input_contract(p.clone(), amulet("in3", P, "100.0"));
        assert_eq!(facts(&twice).consumed, f.consumed);
        // an input holding of the party with a bad amount is an error
        let bad = with_input_contract(
            prepared(
                &["0"],
                vec![exercise_node("0", spend("in7", AMULET_T))],
                vec![],
                &[P],
            ),
            amulet("in7", P, "1e3"),
        );
        assert!(malformed_reason(&bad).contains("initialAmount"));
    }

    fn settle_tx(ex: Exercise) -> PreparedTransaction {
        prepared(&["0"], vec![exercise_node("0", ex)], vec![], &[P])
    }

    /// `ex` with a `fees` argument.
    fn with_fees(mut ex: Exercise, fees: Value) -> Exercise {
        if let Some(Sum::Record(r)) = ex.chosen_value.as_mut().and_then(|v| v.sum.as_mut()) {
            r.fields.push(v2::RecordField {
                label: "fees".into(),
                value: Some(fees),
            });
        }
        ex
    }

    fn fee(receiver: &str, amount: &str) -> Value {
        record(vec![
            ("receiver", party(receiver)),
            ("amount", numeric(amount)),
            ("instrumentId", instrument_id(DSO, "Amulet")),
            ("factoryCid", contract_id("rules")),
            ("extraArgs", record(vec![])),
            ("description", optional(None)),
        ])
    }

    /// The fields of the settle's `quote` argument.
    fn quote_fields(ex: &mut Exercise) -> &mut Vec<v2::RecordField> {
        let Some(Sum::Record(chosen)) = ex.chosen_value.as_mut().and_then(|v| v.sum.as_mut())
        else {
            panic!("the settle argument is not a record");
        };
        let quote = chosen
            .fields
            .iter_mut()
            .find(|f| f.label == "quote")
            .unwrap();
        match quote.value.as_mut().and_then(|v| v.sum.as_mut()) {
            Some(Sum::Record(r)) => &mut r.fields,
            _ => panic!("the quote is not a record"),
        }
    }

    #[test]
    fn a_settle_reads_its_expiry_and_fee_transfers() {
        let settled = |ex: Exercise| facts(&settle_tx(ex)).atomic_settle.unwrap();
        // no fees: the field left out, or None
        let s = settled(settle("v1"));
        assert_eq!((s.valid_until_micros, s.fees), (VALID_UNTIL, vec![]));
        assert!(
            settled(with_fees(settle("v1"), optional(None)))
                .fees
                .is_empty()
        );
        let leg = |receiver: &str, amount: &str| FeeLeg {
            receiver: receiver.into(),
            admin: DSO.into(),
            id: "Amulet".into(),
            amount: dec(amount),
        };
        let fees = optional(Some(list(vec![fee(TRAFFIC, "0.5"), fee(Q, "1.25")])));
        assert_eq!(
            settled(with_fees(settle("v1"), fees)).fees,
            vec![leg(TRAFFIC, "0.5"), leg(Q, "1.25")]
        );

        let bad = |fees: Value| malformed_reason(&settle_tx(with_fees(settle("v1"), fees)));
        assert!(bad(list(vec![])).contains("fees is not an Optional"));
        assert!(bad(optional(Some(text("x")))).contains("fees is not a list"));
        assert!(bad(optional(Some(list(vec![text("x")])))).contains("non-record"));
        assert!(bad(optional(Some(list(vec![fee(Q, "-0.5")])))).contains("negative"));
        let no_receiver = record(vec![
            ("amount", numeric("1.0")),
            ("instrumentId", instrument_id(DSO, "Amulet")),
        ]);
        assert!(
            bad(optional(Some(list(vec![no_receiver])))).contains("missing field receiver")
        );

        // the quote's expiry is required, as an Int64
        let mut ex = settle("v1");
        quote_fields(&mut ex).retain(|f| f.label != "validUntilMicros");
        assert!(malformed_reason(&settle_tx(ex)).contains("missing field validUntilMicros"));
        let mut ex = settle("v1");
        for f in quote_fields(&mut ex)
            .iter_mut()
            .filter(|f| f.label == "validUntilMicros")
        {
            f.value = Some(numeric("1.0"));
        }
        assert!(malformed_reason(&settle_tx(ex)).contains("validUntilMicros is not an Int64"));
    }

    fn pkg(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn in_pkg(mut id: Identifier, n: u8) -> Identifier {
        id.package_id = pkg(n);
        id
    }

    /// One node of each kind with its template and interface in packages 1-6, and two
    /// input contracts created with packages 1 and 7.
    fn packaged_tx() -> PreparedTransaction {
        let mut top = root(RULES, "TransferFactory_Transfer", &["f", "l", "rb", "c"]);
        top.template_id = Some(in_pkg(ident(RULES.0, RULES.1), 1));
        let mut fetched = fetch(in_pkg(ident(CONFIG.0, CONFIG.1), 2), "cs1", &[P]);
        fetched.interface_id = Some(in_pkg(ident("Splice.Api.Token.HoldingV1", "Holding"), 3));
        let mut log = holdings_change("cs1");
        log.template_id = Some(in_pkg(ident(CONFIG.0, CONFIG.1), 4));
        log.interface_id = Some(in_pkg(
            ident("Splice.Api.Token.TransferEventsV2", "EventLog"),
            5,
        ));
        let mut undone = amulet("c2", Q, "1.0");
        undone.template_id = Some(in_pkg(ident(AMULET_T.0, AMULET_T.1), 6));
        let mut payment = amulet("c1", Q, "1.0");
        payment.template_id = Some(in_pkg(ident(AMULET_T.0, AMULET_T.1), 1));
        let p = prepared(
            &["0"],
            vec![
                exercise_node("0", top),
                fetch_node("f", fetched),
                exercise_node("l", log),
                rollback_node("rb", ids(&["u"])),
                create_node("u", undone),
                create_node("c", payment),
            ],
            vec![],
            &[P],
        );
        let mut state = config_state("cs1");
        state.template_id = Some(in_pkg(ident(CONFIG.0, CONFIG.1), 7));
        let mut input = amulet("in1", P, "2.0");
        input.template_id = Some(in_pkg(ident(AMULET_T.0, AMULET_T.1), 1));
        with_input_contract(with_input_contract(p, state), input)
    }

    fn node_mut<'a>(p: &'a mut PreparedTransaction, id: &str) -> &'a mut NodeType {
        let node = p
            .transaction
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|n| n.node_id == id)
            .unwrap();
        match node.versioned_node.as_mut().unwrap() {
            v2::interactive::daml_transaction::node::VersionedNode::V1(n) => {
                n.node_type.as_mut().unwrap()
            }
        }
    }

    fn package_reason(p: &PreparedTransaction) -> String {
        match package_ids(&encode(p)) {
            Err(FactsError::Malformed(reason)) => reason,
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn package_ids_cover_every_node_and_keep_inputs_apart() {
        let found = package_ids(&encode(&packaged_tx())).unwrap();
        assert_eq!(
            found.enforced,
            [pkg(1), pkg(2), pkg(3), pkg(4), pkg(5), pkg(6)]
        );
        assert_eq!(found.inputs, [pkg(1), pkg(7)]);

        let mut p = packaged_tx();
        if let NodeType::Fetch(f) = node_mut(&mut p, "f") {
            f.template_id = None;
        }
        assert!(package_reason(&p).contains("a node has no template id"));
        let mut p = packaged_tx();
        if let Some(InputContract::V1(c)) = p.metadata.as_mut().unwrap().input_contracts[0]
            .contract
            .as_mut()
        {
            c.template_id = None;
        }
        assert!(package_reason(&p).contains("input contract cs1 has no template id"));
        let mut p = packaged_tx();
        p.transaction
            .as_mut()
            .unwrap()
            .nodes
            .push(create_node("orphan", amulet("c7", X, "1.0")));
        assert!(package_reason(&p).contains("no root reaches"));
        assert!(matches!(package_ids(&[0xff]), Err(FactsError::Decode(_))));
    }

    #[test]
    fn a_package_id_that_is_not_64_lowercase_hex_is_an_error() {
        let one = pkg(1);
        for bad in [
            String::new(),
            "pkg".to_string(),
            one[..63].to_string(),
            format!("{one}0"),
            pkg(0xab).to_uppercase(),
            format!("{}g", &one[..63]),
            format!(" {}", &one[..63]),
        ] {
            let set = |id: &mut Option<Identifier>| id.as_mut().unwrap().package_id = bad.clone();
            let mut cases = Vec::new();
            for node in ["0", "f", "l", "u", "c"] {
                let mut p = packaged_tx();
                match node_mut(&mut p, node) {
                    NodeType::Create(c) => set(&mut c.template_id),
                    NodeType::Exercise(e) => set(&mut e.template_id),
                    NodeType::Fetch(f) => set(&mut f.template_id),
                    NodeType::Rollback(_) => panic!("no template on a rollback"),
                }
                cases.push(p);
            }
            for node in ["f", "l"] {
                let mut p = packaged_tx();
                match node_mut(&mut p, node) {
                    NodeType::Exercise(e) => set(&mut e.interface_id),
                    NodeType::Fetch(f) => set(&mut f.interface_id),
                    _ => panic!("no interface on node {node}"),
                }
                cases.push(p);
            }
            let mut p = packaged_tx();
            if let Some(InputContract::V1(c)) = p.metadata.as_mut().unwrap().input_contracts[1]
                .contract
                .as_mut()
            {
                set(&mut c.template_id);
            }
            cases.push(p);
            for p in &cases {
                let reason = package_reason(p);
                assert!(
                    reason.contains("is not 64 lowercase hex"),
                    "{bad:?}: {reason}"
                );
            }
        }
        // the message stays short for a huge id
        let mut p = packaged_tx();
        if let NodeType::Create(c) = node_mut(&mut p, "c") {
            c.template_id.as_mut().unwrap().package_id = "a".repeat(100_000);
        }
        assert!(package_reason(&p).len() < 300);
    }

    #[test]
    fn root_nodes_count_every_root_while_roots_lists_exercises() {
        let mut p = transfer_tx(vec![amulet("c1", Q, "1.0")]);
        assert_eq!(facts(&p).root_nodes, 1);
        let tx = p.transaction.as_mut().unwrap();
        tx.roots.push("r2".into());
        tx.nodes.push(create_node("r2", amulet("c2", Q, "2.0")));
        tx.roots.push("r3".into());
        tx.nodes.push(fetch_node(
            "r3",
            fetch(ident(AMULET_T.0, AMULET_T.1), "c9", &[P]),
        ));
        let f = facts(&p);
        assert_eq!(f.root_nodes, 3);
        assert_eq!(f.roots.len(), 1);
        assert_eq!(f.outflows.len(), 2);
    }

    #[test]
    fn consumed_contracts_neither_created_nor_listed_are_reported() {
        // t1 is created first, in1 is listed, t2 is created only in a rolled-back node,
        // u3 is spent only in a rolled-back node, u1 is spent twice
        let p = prepared(
            &["0"],
            vec![
                exercise_node(
                    "0",
                    root(
                        RULES,
                        "TransferFactory_Transfer",
                        &["c", "a", "t", "b", "d", "d2", "rb", "e"],
                    ),
                ),
                create_node("c", amulet("t1", P, "1.0")),
                exercise_node("a", spend("in1", AMULET_T)),
                exercise_node("t", spend("t1", AMULET_T)),
                exercise_node("b", spend("u2", HOLDING_T)),
                exercise_node("d", spend("u1", AMULET_T)),
                exercise_node("d2", spend("u1", AMULET_T)),
                rollback_node("rb", ids(&["x", "y"])),
                create_node("x", amulet("t2", P, "1.0")),
                exercise_node("y", spend("u3", AMULET_T)),
                exercise_node("e", spend("t2", AMULET_T)),
            ],
            vec![],
            &[P],
        );
        let p = with_input_contract(p, amulet("in1", P, "2.0"));
        let f = facts(&p);
        assert_eq!(f.unlisted_inputs, vec!["t2", "u1", "u2"]);
        assert_eq!(f.consumed, vec![held("in1", DSO, "Amulet", "2")]);
        assert!(f.inflows.is_empty(), "{f:?}");
        assert!(facts(&transfer_tx(vec![amulet("c1", Q, "1.0")]))
            .unlisted_inputs
            .is_empty());
    }

    /// A registry holding with a `lock` field, which may also be left out.
    fn reg_holding_with_lock(cid: &str, owner: &str, amount: &str, lock: Value) -> Create {
        let mut c = reg_holding(cid, owner, amount);
        if let Some(Sum::Record(r)) = c.argument.as_mut().and_then(|v| v.sum.as_mut()) {
            r.fields.push(v2::RecordField {
                label: "lock".into(),
                value: Some(lock),
            });
        }
        c
    }

    #[test]
    fn registry_holdings_report_their_lock() {
        let set = || optional(Some(record(vec![("context", text("held"))])));
        let mut p = transfer_tx(vec![
            reg_holding_with_lock("h1", P, "2.0", set()),
            reg_holding_with_lock("h2", P, "3.0", optional(None)),
            reg_holding("h3", P, "4.0"),
            reg_holding_with_lock("h4", Q, "5.0", set()),
            amulet("c1", P, "6.0"),
        ]);
        add_root(&mut p, "r2", spend("in1", HOLDING_T), vec![]);
        add_root(&mut p, "r3", spend("in2", HOLDING_T), vec![]);
        let p = with_input_contract(p, reg_holding_with_lock("in1", P, "7.0", set()));
        let p = with_input_contract(p, reg_holding("in2", P, "8.0"));
        let f = facts(&p);
        let locked = |h: HoldingFact| HoldingFact { locked: true, ..h };
        assert_eq!(
            f.inflows,
            vec![
                locked(held("h1", REG, "TOK", "2")),
                held("h2", REG, "TOK", "3"),
                held("h3", REG, "TOK", "4"),
                held("c1", DSO, "Amulet", "6"),
            ]
        );
        assert_eq!(
            f.consumed,
            vec![locked(held("in1", REG, "TOK", "7")), held("in2", REG, "TOK", "8")]
        );
        // a lock is not read as a flow: another owner's locked holding is a holding flow
        assert_eq!(f.outflows, vec![flow(Q, REG, "TOK", "5", FlowKind::Holding)]);
        let bad = transfer_tx(vec![reg_holding_with_lock("h1", P, "2.0", text("x"))]);
        assert!(malformed_reason(&bad).contains("lock"));
    }

    #[test]
    fn the_venue_names_are_the_published_constants() {
        let venue = MODELS.iter().find(|m| m.2 == Model::Venue).unwrap();
        assert_eq!(format!("{}:{}", venue.0, venue.1), VENUE_TEMPLATE);
        let f = facts(&settle_tx(settle("v1")));
        assert_eq!(f.roots[0].template, VENUE_TEMPLATE);
        assert_eq!(f.roots[0].choice, SETTLE_CHOICE);
        assert_eq!(f.root_nodes, 1);
    }

    const RESULTS: &str = "Utility.Registry.V0.Holding.Transfer";
    const CREDENTIAL: (&str, &str) = ("Utility.Credential.V0.Credential", "Credential");

    /// A registry transfer payload, as the registry's result contracts record it.
    fn registry_transfer(sender: &str, receiver: &str, amount: &str) -> Value {
        record(vec![
            ("operator", party(REG)),
            ("provider", party(REG)),
            ("registrar", party(REG)),
            ("sender", party(sender)),
            ("receiver", party(receiver)),
            (
                "instrumentIdentifier",
                record(vec![
                    ("source", party(REG)),
                    ("id", text("TOK")),
                    ("scheme", text("s")),
                ]),
            ),
            ("amount", numeric(amount)),
            ("reference", text("")),
            ("batch", record(vec![("id", text("")), ("size", int64(1))])),
        ])
    }

    fn executed_transfer(cid: &str, sender: &str, receiver: &str, amount: &str) -> Create {
        let arg = record(vec![
            ("transfer", registry_transfer(sender, receiver, amount)),
            ("senderLabel", text("")),
            ("receiverLabel", text("")),
            ("observers", optional(None)),
            ("operatorIsObserver", optional(Some(boolean(false)))),
        ]);
        let t = ident(RESULTS, "ExecutedTransfer");
        with_cid(create(t, arg, &[REG]), cid)
    }

    #[test]
    fn registry_result_contracts_and_credential_reads_move_nothing() {
        // both legs of a settle on registries that keep result contracts, and a credential read
        let mut get = root(CREDENTIAL, "Credential_Get", &[]);
        get.contract_id = "cred1".into();
        let mut ex = settle("v1");
        ex.children = ids(&["g", "1", "2", "3", "4"]);
        let p = prepared(
            &["0"],
            vec![
                exercise_node("0", ex),
                exercise_node("g", get),
                create_node("1", reg_holding("h1", Q, "4.0")),
                create_node("2", executed_transfer("e1", P, Q, "4.0")),
                create_node("3", amulet("c1", P, "8.0")),
                create_node("4", executed_transfer("e2", Q, P, "8.0")),
            ],
            vec![],
            &[P],
        );
        let f = facts(&p);
        assert!(f.unmodelled.is_empty(), "{f:?}");
        assert_eq!(
            f.outflows,
            vec![flow(Q, REG, "TOK", "4", FlowKind::Holding)]
        );
        assert_eq!(f.inflows, vec![held("c1", DSO, "Amulet", "8")]);
        assert!(f.consumed.is_empty() && f.unlisted_inputs.is_empty(), "{f:?}");
        // a rejected transfer is a receipt too; the module's other entities stay unmodelled
        let rejected = record(vec![
            ("transfer", registry_transfer(Q, P, "2.0")),
            ("reason", text("")),
            ("observers", optional(None)),
            ("operatorIsObserver", optional(Some(boolean(false)))),
        ]);
        let failed = record(vec![("transfer", registry_transfer(P, Q, "2.0"))]);
        let f = facts(&transfer_tx(vec![
            with_cid(
                create(ident(RESULTS, "RejectedTransfer"), rejected, &[Q, P]),
                "r1",
            ),
            with_cid(
                create(ident(RESULTS, "FailedTransfer"), failed, &[REG]),
                "f1",
            ),
        ]));
        assert_eq!(
            f.unmodelled,
            ["Utility.Registry.V0.Holding.Transfer:FailedTransfer"]
        );
        assert!(f.outflows.is_empty(), "{f:?}");
    }

    #[test]
    fn the_partys_consumed_locked_amulets_are_consumed_holdings() {
        const LOCKED_T: (&str, &str) = ("Splice.Amulet", "LockedAmulet");
        // k1 is the party's, k2 another owner's, k3 is created and spent in the transaction
        let p = prepared(
            &["0"],
            vec![
                exercise_node(
                    "0",
                    root(
                        RULES,
                        "TransferFactory_Transfer",
                        &["a", "b", "l", "c", "d", "e"],
                    ),
                ),
                exercise_node("a", spend("k1", LOCKED_T)),
                exercise_node("b", spend("k2", LOCKED_T)),
                create_node("l", locked("k3", P, "1.0", &[DSO])),
                exercise_node("c", spend("k3", LOCKED_T)),
                create_node("d", amulet("c1", Q, "300.0")),
                create_node("e", amulet("c2", P, "2.0")),
            ],
            vec![],
            &[P],
        );
        let p = with_input_contract(p, locked("k1", P, "302.0", &[DSO]));
        let p = with_input_contract(p, locked("k2", Q, "9.0", &[DSO]));
        let f = facts(&p);
        assert_eq!(
            f.consumed,
            vec![HoldingFact {
                locked: true,
                ..held("k1", DSO, "Amulet", "302")
            }]
        );
        assert_eq!(
            f.outflows,
            vec![flow(Q, DSO, "Amulet", "300", FlowKind::Holding)]
        );
        assert_eq!(f.inflows, vec![held("c2", DSO, "Amulet", "2")]);
        assert!(f.unlisted_inputs.is_empty(), "{f:?}");
        // the party's locked input is read in full
        let bad = with_input_contract(
            prepared(
                &["0"],
                vec![exercise_node("0", spend("k4", LOCKED_T))],
                vec![],
                &[P],
            ),
            locked("k4", P, "1e3", &[DSO]),
        );
        assert!(malformed_reason(&bad).contains("initialAmount"));
    }

    #[test]
    fn new_readers_of_mangled_bytes_never_panic() {
        // only packaged_tx uses real package ids
        let fees = optional(Some(list(vec![fee(Q, "0.5")])));
        for (p, packaged) in [
            (packaged_tx(), true),
            (prepay_tx(), false),
            (settle_tx(with_fees(settle("v1"), fees)), false),
        ] {
            let bytes = encode(&p);
            assert_eq!(package_ids(&bytes).is_ok(), packaged);
            assert!(extract_facts(&bytes, P).is_ok());
            for n in 0..bytes.len() {
                let _ = package_ids(&bytes[..n]);
                let _ = extract_facts(&bytes[..n], P);
            }
            for i in 0..bytes.len() {
                for mask in [0x01u8, 0x80, 0xff] {
                    let mut b = bytes.clone();
                    b[i] ^= mask;
                    let _ = package_ids(&b);
                    let _ = extract_facts(&b, P);
                }
            }
        }
    }
}
