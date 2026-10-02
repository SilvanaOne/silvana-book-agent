//! Transaction hash computation
//!
//! Computes the 3-layer SHA-256 hash per TX_VERIFICATION.md:
//!   Layer 1: transaction_hash = SHA-256(purpose || version || root_node_hashes)
//!   Layer 2: metadata_hash = SHA-256(purpose || encoding_version || act_as || ... || input_contracts)
//!   Layer 3: final_hash = SHA-256(purpose || 0x02 || transaction_hash || metadata_hash)
//!
//! On error, returns `[0u8; 32]` sentinel so the caller falls back to the server hash.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context, Result};
use prost::Message;
use sha2::{Digest, Sha256};
use tracing::{debug, error};

use crate::text::short;

use crate::decode::canton_proto::com::daml::ledger::api::v2 as proto_v2;
use proto_v2::interactive::{
    daml_transaction, metadata::input_contract::Contract as InputContractOneof, PreparedTransaction,
};
use proto_v2::interactive::transaction::v1::{node::NodeType, Create, Exercise, Fetch, Rollback};
use proto_v2::{value::Sum, Identifier, Value};

// ============================================================================
// Constants
// ============================================================================

/// HashPurpose.PreparedSubmission = 48
const HASH_PURPOSE: [u8; 4] = [0x00, 0x00, 0x00, 0x30];

/// Node encoding version (always 1)
const NODE_ENCODING_V1: u8 = 0x01;

/// Metadata encoding version (always 1)
const METADATA_ENCODING_V1: u8 = 0x01;

/// Hashing scheme version in final hash — ALWAYS V2 even for V3 metadata
const HASHING_SCHEME_V2: u8 = 0x02;

/// Deepest node nesting followed (roots are level 1).
const MAX_NODE_DEPTH: usize = 256;

/// Deepest value nesting encoded (a top-level value is level 1).
const MAX_VALUE_DEPTH: usize = 128;

/// Node type tags
const CREATE_TAG: u8 = 0x00;
const EXERCISE_TAG: u8 = 0x01;
const FETCH_TAG: u8 = 0x02;
const ROLLBACK_TAG: u8 = 0x03;

/// Value type tags
const UNIT_TAG: u8 = 0x00;
const BOOL_TAG: u8 = 0x01;
const INT64_TAG: u8 = 0x02;
const NUMERIC_TAG: u8 = 0x03;
const TIMESTAMP_TAG: u8 = 0x04;
const DATE_TAG: u8 = 0x05;
const PARTY_TAG: u8 = 0x06;
const TEXT_TAG: u8 = 0x07;
const CONTRACT_ID_TAG: u8 = 0x08;
const OPTIONAL_TAG: u8 = 0x09;
const LIST_TAG: u8 = 0x0A;
const TEXT_MAP_TAG: u8 = 0x0B;
const RECORD_TAG: u8 = 0x0C;
const VARIANT_TAG: u8 = 0x0D;
const ENUM_TAG: u8 = 0x0E;
const GEN_MAP_TAG: u8 = 0x0F;

// ============================================================================
// Hash accumulator (builder pattern)
// ============================================================================

/// Accumulates bytes for deterministic hashing.
struct Acc(Vec<u8>);

impl Acc {
    fn new() -> Self {
        Self(Vec::with_capacity(4096))
    }

    fn byte(&mut self, b: u8) -> &mut Self {
        self.0.push(b);
        self
    }

    fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }

    fn bool_val(&mut self, v: bool) -> &mut Self {
        self.byte(if v { 1 } else { 0 })
    }

    fn i32_val(&mut self, v: i32) -> &mut Self {
        self.raw(&v.to_be_bytes())
    }

    fn i64_val(&mut self, v: i64) -> &mut Self {
        self.raw(&v.to_be_bytes())
    }

    /// Encode proto uint32 as signed i32 (matches Java's signed int encoding)
    fn u32_val(&mut self, v: u32) -> &mut Self {
        self.i32_val(v as i32)
    }

    /// Encode proto uint64 as signed i64 (matches Java's signed long encoding)
    fn u64_val(&mut self, v: u64) -> &mut Self {
        self.i64_val(v as i64)
    }

    /// Length-prefixed UTF-8 string
    fn str_val(&mut self, s: &str) -> &mut Self {
        self.i32_val(s.len() as i32);
        self.raw(s.as_bytes())
    }

    /// Length-prefixed raw bytes
    fn bytes_val(&mut self, b: &[u8]) -> &mut Self {
        self.i32_val(b.len() as i32);
        self.raw(b)
    }

    /// Raw 32-byte hash (NO length prefix — fixed size)
    fn hash_val(&mut self, h: &[u8; 32]) -> &mut Self {
        self.raw(h)
    }

    /// Decode hex string to bytes, then encode as length-prefixed bytes
    fn hex_bytes(&mut self, hex_str: &str) -> Result<&mut Self> {
        let decoded = hex::decode(hex_str)
            .with_context(|| format!("invalid hex: {}...", short(hex_str, 20)))?;
        Ok(self.bytes_val(&decoded))
    }

    /// Sorted string set: sort then encode as repeated string
    fn string_set(&mut self, vals: &[String]) -> &mut Self {
        let mut sorted: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
        sorted.sort();
        self.i32_val(sorted.len() as i32);
        for s in sorted {
            self.str_val(s);
        }
        self
    }

    fn finish(self) -> [u8; 32] {
        sha256(&self.0)
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

// ============================================================================
// Identifier encoding
// ============================================================================

fn encode_identifier(acc: &mut Acc, id: &Identifier) {
    acc.str_val(&id.package_id);

    let mod_parts: Vec<&str> = id.module_name.split('.').collect();
    acc.i32_val(mod_parts.len() as i32);
    for p in &mod_parts {
        acc.str_val(p);
    }

    let ent_parts: Vec<&str> = id.entity_name.split('.').collect();
    acc.i32_val(ent_parts.len() as i32);
    for p in &ent_parts {
        acc.str_val(p);
    }
}

/// Encode optional identifier: 0x00 if None, 0x01 + identifier if Some
fn encode_optional_identifier(acc: &mut Acc, id: &Option<Identifier>) {
    match id {
        Some(id) => {
            acc.byte(0x01);
            encode_identifier(acc, id);
        }
        None => {
            acc.byte(0x00);
        }
    }
}

// ============================================================================
// Value encoding (16 Daml LF types)
// ============================================================================

fn encode_value(acc: &mut Acc, value: &Value) -> Result<()> {
    encode_value_at(acc, value, 1)
}

fn encode_value_at(acc: &mut Acc, value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_VALUE_DEPTH {
        bail!("value nesting exceeds {MAX_VALUE_DEPTH} levels");
    }
    let next = depth.saturating_add(1);
    let sum = value.sum.as_ref().context("Value has no sum field set")?;

    match sum {
        Sum::Unit(_) => {
            acc.byte(UNIT_TAG);
        }
        Sum::Bool(b) => {
            acc.byte(BOOL_TAG);
            acc.bool_val(*b);
        }
        Sum::Int64(v) => {
            acc.byte(INT64_TAG);
            acc.i64_val(*v);
        }
        Sum::Numeric(s) => {
            acc.byte(NUMERIC_TAG);
            acc.str_val(s);
        }
        Sum::Timestamp(v) => {
            acc.byte(TIMESTAMP_TAG);
            acc.i64_val(*v);
        }
        Sum::Date(v) => {
            acc.byte(DATE_TAG);
            acc.i32_val(*v);
        }
        Sum::Party(s) => {
            acc.byte(PARTY_TAG);
            acc.str_val(s);
        }
        Sum::Text(s) => {
            acc.byte(TEXT_TAG);
            acc.str_val(s);
        }
        Sum::ContractId(hex_str) => {
            // Contract IDs are hex strings — decode to bytes first
            acc.byte(CONTRACT_ID_TAG);
            acc.hex_bytes(hex_str)?;
        }
        Sum::Optional(opt) => {
            acc.byte(OPTIONAL_TAG);
            match &opt.value {
                Some(inner) => {
                    acc.byte(0x01);
                    encode_value_at(acc, inner, next)?;
                }
                None => {
                    acc.byte(0x00);
                }
            }
        }
        Sum::List(list) => {
            acc.byte(LIST_TAG);
            acc.i32_val(list.elements.len() as i32);
            for elem in &list.elements {
                encode_value_at(acc, elem, next)?;
            }
        }
        Sum::TextMap(map) => {
            acc.byte(TEXT_MAP_TAG);
            acc.i32_val(map.entries.len() as i32);
            for entry in &map.entries {
                acc.str_val(&entry.key);
                encode_value_at(
                    acc,
                    entry.value.as_ref().context("TextMap entry missing value")?,
                    next,
                )?;
            }
        }
        Sum::Record(record) => {
            acc.byte(RECORD_TAG);
            encode_optional_identifier(acc, &record.record_id);
            acc.i32_val(record.fields.len() as i32);
            for field in &record.fields {
                // Optional field label: empty string means None
                if field.label.is_empty() {
                    acc.byte(0x00);
                } else {
                    acc.byte(0x01);
                    acc.str_val(&field.label);
                }
                encode_value_at(
                    acc,
                    field.value.as_ref().context("Record field missing value")?,
                    next,
                )?;
            }
        }
        Sum::Variant(variant) => {
            acc.byte(VARIANT_TAG);
            encode_optional_identifier(acc, &variant.variant_id);
            acc.str_val(&variant.constructor);
            encode_value_at(
                acc,
                variant.value.as_ref().context("Variant missing value")?,
                next,
            )?;
        }
        Sum::Enum(e) => {
            acc.byte(ENUM_TAG);
            encode_optional_identifier(acc, &e.enum_id);
            acc.str_val(&e.constructor);
        }
        Sum::GenMap(map) => {
            acc.byte(GEN_MAP_TAG);
            acc.i32_val(map.entries.len() as i32);
            for entry in &map.entries {
                encode_value_at(
                    acc,
                    entry.key.as_ref().context("GenMap entry missing key")?,
                    next,
                )?;
                encode_value_at(
                    acc,
                    entry.value.as_ref().context("GenMap entry missing value")?,
                    next,
                )?;
            }
        }
    }

    Ok(())
}

// ============================================================================
// Node seed lookup
// ============================================================================

type SeedMap<'a> = HashMap<String, &'a [u8]>;

/// Seeds keyed by `NodeSeed.node_id` rendered as decimal, matching
/// `DamlNode.node_id` text exactly; the first entry for an id wins.
fn build_seed_map(node_seeds: &[daml_transaction::NodeSeed]) -> SeedMap<'_> {
    let mut map = HashMap::new();
    for ns in node_seeds {
        map.entry(ns.node_id.to_string()).or_insert(ns.seed.as_slice());
    }
    map
}

// ============================================================================
// Node encoding
// ============================================================================

type NodesDict<'a> = HashMap<String, &'a daml_transaction::Node>;

/// Lookup tables for one transaction plus the node ids already hashed in it.
struct NodeWalk<'a> {
    nodes: NodesDict<'a>,
    seeds: SeedMap<'a>,
    seen: HashSet<&'a str>,
}

/// Encode a Create node (without NodeEncodingVersion prefix).
/// Used both for transaction nodes and input contract (disclosed) nodes.
fn encode_create_node(acc: &mut Acc, create: &Create, seed: Option<&[u8]>) -> Result<()> {
    acc.str_val(&create.lf_version);
    acc.byte(CREATE_TAG);

    // Optional seed
    match seed {
        Some(s) => {
            acc.byte(0x01);
            acc.raw(s); // raw 32 bytes, no length prefix
        }
        None => {
            acc.byte(0x00);
        }
    }

    acc.hex_bytes(&create.contract_id)?;
    acc.str_val(&create.package_name);
    encode_identifier(
        acc,
        create
            .template_id
            .as_ref()
            .context("Create missing template_id")?,
    );
    encode_value(
        acc,
        create.argument.as_ref().context("Create missing argument")?,
    )?;
    acc.string_set(&create.signatories);
    acc.string_set(&create.stakeholders);

    Ok(())
}

/// Encode an Exercise node
fn encode_exercise_node<'a>(
    acc: &mut Acc,
    exercise: &'a Exercise,
    node_id: &str,
    walk: &mut NodeWalk<'a>,
    depth: usize,
) -> Result<()> {
    let seed = walk
        .seeds
        .get(node_id)
        .copied()
        .context("Exercise node must have a seed")?;

    acc.str_val(&exercise.lf_version);
    acc.byte(EXERCISE_TAG);

    // Required seed — raw bytes, NOT optional-wrapped
    acc.raw(seed);

    acc.hex_bytes(&exercise.contract_id)?;
    acc.str_val(&exercise.package_name);
    encode_identifier(
        acc,
        exercise
            .template_id
            .as_ref()
            .context("Exercise missing template_id")?,
    );
    acc.string_set(&exercise.signatories);
    acc.string_set(&exercise.stakeholders);
    acc.string_set(&exercise.acting_parties);

    // Optional interface_id
    encode_optional_identifier(acc, &exercise.interface_id);

    acc.str_val(&exercise.choice_id);
    encode_value(
        acc,
        exercise
            .chosen_value
            .as_ref()
            .context("Exercise missing chosen_value")?,
    )?;
    acc.bool_val(exercise.consuming);

    // Optional exercise_result
    match &exercise.exercise_result {
        Some(v) => {
            acc.byte(0x01);
            encode_value(acc, v)?;
        }
        None => {
            acc.byte(0x00);
        }
    }

    acc.string_set(&exercise.choice_observers);

    // Children — recursive
    encode_node_ids(acc, &exercise.children, walk, depth.saturating_add(1))?;

    Ok(())
}

/// Encode a Fetch node
fn encode_fetch_node(acc: &mut Acc, fetch: &Fetch) -> Result<()> {
    acc.str_val(&fetch.lf_version);
    acc.byte(FETCH_TAG);

    acc.hex_bytes(&fetch.contract_id)?;
    acc.str_val(&fetch.package_name);
    encode_identifier(
        acc,
        fetch
            .template_id
            .as_ref()
            .context("Fetch missing template_id")?,
    );
    acc.string_set(&fetch.signatories);
    acc.string_set(&fetch.stakeholders);

    // Optional interface_id
    encode_optional_identifier(acc, &fetch.interface_id);

    acc.string_set(&fetch.acting_parties);

    Ok(())
}

/// Encode a Rollback node (no lf_version)
fn encode_rollback_node<'a>(
    acc: &mut Acc,
    rollback: &'a Rollback,
    walk: &mut NodeWalk<'a>,
    depth: usize,
) -> Result<()> {
    acc.byte(ROLLBACK_TAG);
    encode_node_ids(acc, &rollback.children, walk, depth.saturating_add(1))?;
    Ok(())
}

/// Encode a full node: NodeEncodingVersion + node-type-specific encoding, then SHA-256
fn hash_node<'a>(
    daml_node: &'a daml_transaction::Node,
    walk: &mut NodeWalk<'a>,
    depth: usize,
) -> Result<[u8; 32]> {
    let versioned = daml_node
        .versioned_node
        .as_ref()
        .context("Node missing versioned_node")?;

    let v1_node = match versioned {
        daml_transaction::node::VersionedNode::V1(n) => n,
    };

    let node_type = v1_node
        .node_type
        .as_ref()
        .context("v1 Node missing node_type")?;

    let mut acc = Acc::new();
    acc.byte(NODE_ENCODING_V1);

    match node_type {
        NodeType::Create(create) => {
            let seed = walk.seeds.get(daml_node.node_id.as_str()).copied();
            encode_create_node(&mut acc, create, seed)?;
        }
        NodeType::Exercise(exercise) => {
            encode_exercise_node(&mut acc, exercise, &daml_node.node_id, walk, depth)?;
        }
        NodeType::Fetch(fetch) => {
            encode_fetch_node(&mut acc, fetch)?;
        }
        NodeType::Rollback(rollback) => {
            encode_rollback_node(&mut acc, rollback, walk, depth)?;
        }
    }

    let h = acc.finish();
    let tag = match node_type {
        NodeType::Create(_) => "Create",
        NodeType::Exercise(_) => "Exercise",
        NodeType::Fetch(_) => "Fetch",
        NodeType::Rollback(_) => "Rollback",
    };
    debug!("  node[{}] ({}) hash: {}", daml_node.node_id, tag, hex::encode(h));
    Ok(h)
}

/// Encode a list of node IDs as repeated hashed nodes. Each node may be
/// referenced once per transaction, at most `MAX_NODE_DEPTH` levels deep.
fn encode_node_ids<'a>(
    acc: &mut Acc,
    node_ids: &'a [String],
    walk: &mut NodeWalk<'a>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_NODE_DEPTH && !node_ids.is_empty() {
        bail!("transaction nesting exceeds {MAX_NODE_DEPTH} levels");
    }
    acc.i32_val(node_ids.len() as i32);
    for node_id in node_ids {
        if !walk.seen.insert(node_id.as_str()) {
            bail!("Node '{node_id}' referenced more than once");
        }
        let daml_node = walk
            .nodes
            .get(node_id.as_str())
            .copied()
            .with_context(|| format!("Node '{node_id}' not found in transaction"))?;
        let h = hash_node(daml_node, walk, depth)?;
        acc.hash_val(&h);
    }
    Ok(())
}

// ============================================================================
// Layer 1: Transaction hash
// ============================================================================

fn hash_transaction(tx: &proto_v2::interactive::DamlTransaction) -> Result<[u8; 32]> {
    let mut walk = NodeWalk {
        nodes: build_nodes_dict(tx),
        seeds: build_seed_map(&tx.node_seeds),
        seen: HashSet::new(),
    };

    let mut acc = Acc::new();
    acc.raw(&HASH_PURPOSE);
    acc.str_val(&tx.version);
    encode_node_ids(&mut acc, &tx.roots, &mut walk, 1)?;

    let h = acc.finish();
    debug!("Layer 1 (tx_hash): {}", hex::encode(h));
    Ok(h)
}

// ============================================================================
// Layer 2: Metadata hash
// ============================================================================

fn hash_metadata(
    metadata: &proto_v2::interactive::Metadata,
    is_v3: bool,
) -> Result<[u8; 32]> {
    let submitter = metadata
        .submitter_info
        .as_ref()
        .context("Metadata missing submitter_info")?;

    let mut acc = Acc::new();
    acc.raw(&HASH_PURPOSE);
    acc.byte(METADATA_ENCODING_V1);

    // Act As parties (SORTED)
    acc.string_set(&submitter.act_as);

    // Command ID
    acc.str_val(&submitter.command_id);

    // Transaction UUID
    acc.str_val(&metadata.transaction_uuid);

    // Mediator group (uint32 → i32)
    acc.u32_val(metadata.mediator_group);

    // Synchronizer ID
    acc.str_val(&metadata.synchronizer_id);

    // Optional min_ledger_effective_time
    match metadata.min_ledger_effective_time {
        Some(v) => {
            acc.byte(0x01);
            acc.u64_val(v);
        }
        None => {
            acc.byte(0x00);
        }
    }

    // Optional max_ledger_effective_time
    match metadata.max_ledger_effective_time {
        Some(v) => {
            acc.byte(0x01);
            acc.u64_val(v);
        }
        None => {
            acc.byte(0x00);
        }
    }

    // Preparation time (uint64)
    acc.u64_val(metadata.preparation_time);

    // Input contracts (disclosed contracts)
    acc.i32_val(metadata.input_contracts.len() as i32);
    for ic in &metadata.input_contracts {
        acc.u64_val(ic.created_at);

        let create = match &ic.contract {
            Some(InputContractOneof::V1(c)) => c,
            None => bail!("Input contract missing v1 create"),
        };

        // Hash the create node with no seed (disclosed contracts have no seed)
        let mut create_acc = Acc::new();
        create_acc.byte(NODE_ENCODING_V1);
        encode_create_node(&mut create_acc, create, None)?;
        let create_hash = create_acc.finish();
        debug!("  input_contract[{}] hash: {} (created_at={})",
            short(&create.contract_id, 16),
            hex::encode(create_hash), ic.created_at);
        acc.hash_val(&create_hash);
    }

    // V3 only: max_record_time
    if is_v3 {
        match metadata.max_record_time {
            Some(v) => {
                acc.byte(0x01);
                acc.u64_val(v);
            }
            None => {
                acc.byte(0x00);
            }
        }
    }

    let h = acc.finish();
    debug!("Layer 2 (meta_hash): {}", hex::encode(h));
    Ok(h)
}

// ============================================================================
// Layer 3: Final hash
// ============================================================================

fn compute_final_hash(
    prepared: &PreparedTransaction,
    hashing_scheme_version: &str,
) -> Result<[u8; 32]> {
    let tx = prepared
        .transaction
        .as_ref()
        .context("PreparedTransaction missing transaction")?;
    let metadata = prepared
        .metadata
        .as_ref()
        .context("PreparedTransaction missing metadata")?;

    let is_v3 = parse_hashing_version(hashing_scheme_version);

    let tx_hash = hash_transaction(tx)?;
    let meta_hash = hash_metadata(metadata, is_v3)?;

    let mut acc = Acc::new();
    acc.raw(&HASH_PURPOSE);
    acc.byte(HASHING_SCHEME_V2); // ALWAYS V2 for outer hash
    acc.hash_val(&tx_hash);
    acc.hash_val(&meta_hash);

    let h = acc.finish();
    debug!("Layer 3 (final_hash): {}", hex::encode(h));
    Ok(h)
}

// ============================================================================
// Helpers
// ============================================================================

fn build_nodes_dict(tx: &proto_v2::interactive::DamlTransaction) -> NodesDict<'_> {
    let mut dict = HashMap::new();
    for node in &tx.nodes {
        dict.insert(node.node_id.clone(), node);
    }
    dict
}

/// Parse hashing scheme version string → is_v3 bool
fn parse_hashing_version(s: &str) -> bool {
    matches!(s, "HASHING_SCHEME_VERSION_V3" | "V3" | "3")
}

// ============================================================================
// Public entry point
// ============================================================================

/// Compute the transaction hash from `PreparedTransaction` bytes.
///
/// Returns `[0u8; 32]` sentinel on error — caller detects and uses server hash.
pub fn compute_hash(
    prepared_transaction_bytes: &[u8],
    hashing_scheme_version: &str,
) -> Result<[u8; 32]> {
    let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
        .context("Failed to decode PreparedTransaction")?;

    match compute_final_hash(&prepared, hashing_scheme_version) {
        Ok(hash) => {
            debug!(
                "TX HASH computed: {} (scheme={})",
                hex::encode(hash),
                hashing_scheme_version
            );
            Ok(hash)
        }
        Err(e) => {
            error!("TX HASH computation failed: {:#}", e);
            Ok([0u8; 32]) // sentinel → caller falls back to server hash
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use std::time::{Duration, Instant};

    const SENTINEL: [u8; 32] = [0u8; 32];

    fn hash(p: &PreparedTransaction) -> [u8; 32] {
        compute_hash(&encode(p), "V2").expect("compute_hash only fails on undecodable bytes")
    }

    fn leaf(id: &str) -> Node {
        create_node(id, create(ident("M", "T"), boolean(true), &["p"]))
    }

    /// Exercise chain 0 -> 1 -> ... -> len-1, every node seeded.
    fn chain(len: usize) -> PreparedTransaction {
        let nodes = (0..len)
            .map(|i| {
                let children = if i + 1 < len { vec![(i + 1).to_string()] } else { vec![] };
                exercise_node(&i.to_string(), exercise("C", &["p"], children))
            })
            .collect();
        let seeds = (0..len).map(|i| seed(i as i32, 1)).collect();
        prepared(&["0"], nodes, seeds, &["p"])
    }

    #[test]
    fn plain_tree_hashes() {
        let p = prepared(
            &["0"],
            vec![exercise_node("0", exercise("C", &["p"], ids(&["1", "2"]))), leaf("1"), leaf("2")],
            vec![seed(0, 1)],
            &["p"],
        );
        let h = hash(&p);
        assert_ne!(h, SENTINEL);
        assert_eq!(hash(&p), h, "deterministic");
    }

    #[test]
    fn self_referencing_node_returns_sentinel() {
        let p = prepared(
            &["0"],
            vec![exercise_node("0", exercise("C", &["p"], ids(&["0"])))],
            vec![seed(0, 0)],
            &["p"],
        );
        assert_eq!(hash(&p), SENTINEL);
    }

    #[test]
    fn two_node_cycle_through_rollback_returns_sentinel() {
        let p = prepared(
            &["0"],
            vec![exercise_node("0", exercise("C", &["p"], ids(&["1"]))), rollback_node("1", ids(&["0"]))],
            vec![seed(0, 0)],
            &["p"],
        );
        assert_eq!(hash(&p), SENTINEL);
    }

    #[test]
    fn shared_child_returns_sentinel() {
        let p = prepared(
            &["0"],
            vec![exercise_node("0", exercise("C", &["p"], ids(&["1", "1"]))), leaf("1")],
            vec![seed(0, 1)],
            &["p"],
        );
        assert_eq!(hash(&p), SENTINEL);
    }

    #[test]
    fn duplicate_root_returns_sentinel() {
        let p = prepared(&["0", "0"], vec![leaf("0")], vec![], &["p"]);
        assert_eq!(hash(&p), SENTINEL);
    }

    #[test]
    fn diamond_dag_is_not_expanded() {
        // node i -> [i+1, i+1]: expanding it would hash 2^24 leaves
        const LEVELS: usize = 24;
        let mut nodes: Vec<Node> = (0..LEVELS)
            .map(|i| {
                let next = (i + 1).to_string();
                exercise_node(&i.to_string(), exercise("C", &["p"], vec![next.clone(), next]))
            })
            .collect();
        nodes.push(leaf(&LEVELS.to_string()));
        let seeds = (0..LEVELS).map(|i| seed(i as i32, 1)).collect();
        let p = prepared(&["0"], nodes, seeds, &["p"]);
        let start = Instant::now();
        assert_eq!(hash(&p), SENTINEL);
        assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
    }

    #[test]
    fn nesting_at_the_limit_hashes_within_a_small_stack() {
        let p = chain(MAX_NODE_DEPTH);
        let h = std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(move || hash(&p))
            .unwrap()
            .join()
            .unwrap();
        assert_ne!(h, SENTINEL);
    }

    #[test]
    fn nesting_past_the_limit_returns_sentinel() {
        assert_eq!(hash(&chain(MAX_NODE_DEPTH + 1)), SENTINEL);
    }

    #[test]
    fn very_deep_chain_returns_sentinel() {
        assert_eq!(hash(&chain(20_000)), SENTINEL);
    }

    #[test]
    fn rollback_counts_towards_nesting() {
        // 255 exercises plus a rollback at level 256 holding a leaf at level 257
        let mut p = chain(MAX_NODE_DEPTH - 1);
        let tx = p.transaction.as_mut().unwrap();
        let last = (MAX_NODE_DEPTH - 2).to_string();
        for n in tx.nodes.iter_mut().filter(|n| n.node_id == last) {
            *n = exercise_node(&last, exercise("C", &["p"], ids(&["rb"])));
        }
        tx.nodes.push(rollback_node("rb", ids(&["leaf"])));
        tx.nodes.push(leaf("leaf"));
        assert_eq!(hash(&p), SENTINEL);
        // the same rollback without a child stays within the limit
        let tx = p.transaction.as_mut().unwrap();
        for n in tx.nodes.iter_mut().filter(|n| n.node_id == "rb") {
            *n = rollback_node("rb", vec![]);
        }
        assert_ne!(hash(&p), SENTINEL);
    }

    #[test]
    fn many_nodes_and_seeds_hash_in_linear_time() {
        // 10k unseeded creates under one exercise plus 10k unrelated seeds
        const N: usize = 10_000;
        let mut nodes: Vec<Node> = (1..=N).map(|i| leaf(&i.to_string())).collect();
        let children = (1..=N).map(|i| i.to_string()).collect();
        nodes.push(exercise_node("0", exercise("C", &["p"], children)));
        let mut seeds: Vec<NodeSeed> = (1..=N).map(|i| seed(-(i as i32), 2)).collect();
        seeds.push(seed(0, 1));
        let p = prepared(&["0"], nodes, seeds, &["p"]);
        let start = Instant::now();
        assert_ne!(hash(&p), SENTINEL);
        assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
    }

    #[test]
    fn first_seed_entry_wins() {
        let tx_with = |seeds: Vec<NodeSeed>| {
            prepared(&["0"], vec![exercise_node("0", exercise("C", &["p"], vec![]))], seeds, &["p"])
        };
        let first = hash(&tx_with(vec![seed(0, 1), seed(0, 2)]));
        assert_ne!(first, SENTINEL);
        assert_eq!(first, hash(&tx_with(vec![seed(0, 1)])));
        assert_ne!(first, hash(&tx_with(vec![seed(0, 2)])));
    }

    #[test]
    fn seed_ids_match_the_decimal_text_only() {
        let tx_with = |node_id: &str, seeds: Vec<NodeSeed>| {
            prepared(&[node_id], vec![leaf(node_id)], seeds, &["p"])
        };
        let unseeded = hash(&tx_with("05", vec![]));
        assert_eq!(hash(&tx_with("05", vec![seed(5, 9)])), unseeded, "\"05\" is not seed 5");
        assert_ne!(hash(&tx_with("5", vec![seed(5, 9)])), hash(&tx_with("5", vec![])));
    }

    #[test]
    fn value_nesting_limit() {
        let nested = |levels: usize| {
            let mut v = boolean(true);
            for _ in 1..levels {
                v = optional(Some(v));
            }
            v
        };
        assert!(encode_value(&mut Acc::new(), &nested(MAX_VALUE_DEPTH)).is_ok());
        let err = encode_value(&mut Acc::new(), &nested(MAX_VALUE_DEPTH + 1)).unwrap_err();
        assert!(err.to_string().contains("value nesting"), "{err}");
        let wide = list(vec![nested(MAX_VALUE_DEPTH - 1), record(vec![("f", nested(MAX_VALUE_DEPTH))])]);
        assert!(encode_value(&mut Acc::new(), &wide).is_err());
    }

    #[test]
    fn non_hex_multibyte_contract_id_is_an_error() {
        let bad = "€".repeat(10);
        let err = Acc::new().hex_bytes(&bad).err().expect("not hex");
        assert_eq!(err.to_string(), format!("invalid hex: {}...", "€".repeat(10)));
        let mut c = create(ident("M", "T"), boolean(true), &["p"]);
        c.contract_id = "€".repeat(30);
        let p = prepared(&["0"], vec![create_node("0", c)], vec![], &["p"]);
        assert_eq!(hash(&p), SENTINEL);
    }
}
