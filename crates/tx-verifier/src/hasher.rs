//! Transaction hash computation
//!
//! Computes the 3-layer SHA-256 hash per TX_VERIFICATION.md:
//!   Layer 1: transaction_hash = SHA-256(purpose || version || root_node_hashes)
//!   Layer 2: metadata_hash = SHA-256(purpose || encoding_version || act_as || ... || input_contracts)
//!   Layer 3: final_hash = SHA-256(purpose || 0x02 || transaction_hash || metadata_hash)
//!
//! On error, returns `[0u8; 32]` sentinel so the caller falls back to the server hash.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use prost::Message;
use sha2::{Digest, Sha256};
use tracing::{debug, error};

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
            .with_context(|| format!("invalid hex: {}...", &hex_str[..hex_str.len().min(20)]))?;
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
                    encode_value(acc, inner)?;
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
                encode_value(acc, elem)?;
            }
        }
        Sum::TextMap(map) => {
            acc.byte(TEXT_MAP_TAG);
            acc.i32_val(map.entries.len() as i32);
            for entry in &map.entries {
                acc.str_val(&entry.key);
                encode_value(
                    acc,
                    entry.value.as_ref().context("TextMap entry missing value")?,
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
                encode_value(
                    acc,
                    field.value.as_ref().context("Record field missing value")?,
                )?;
            }
        }
        Sum::Variant(variant) => {
            acc.byte(VARIANT_TAG);
            encode_optional_identifier(acc, &variant.variant_id);
            acc.str_val(&variant.constructor);
            encode_value(
                acc,
                variant.value.as_ref().context("Variant missing value")?,
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
                encode_value(
                    acc,
                    entry.key.as_ref().context("GenMap entry missing key")?,
                )?;
                encode_value(
                    acc,
                    entry.value.as_ref().context("GenMap entry missing value")?,
                )?;
            }
        }
    }

    Ok(())
}

// ============================================================================
// Node seed lookup
// ============================================================================

/// Find seed for a node by matching NodeSeed.node_id (i32) to DamlNode.node_id (String)
fn find_seed(node_id: &str, node_seeds: &[daml_transaction::NodeSeed]) -> Option<Vec<u8>> {
    for ns in node_seeds {
        if ns.node_id.to_string() == node_id {
            return Some(ns.seed.clone());
        }
    }
    None
}

// ============================================================================
// Node encoding
// ============================================================================

type NodesDict<'a> = HashMap<String, &'a daml_transaction::Node>;

/// Encode a Create node (without NodeEncodingVersion prefix).
/// Used both for transaction nodes and input contract (disclosed) nodes.
fn encode_create_node(
    acc: &mut Acc,
    create: &Create,
    node_id: &str,
    node_seeds: &[daml_transaction::NodeSeed],
) -> Result<()> {
    let seed = find_seed(node_id, node_seeds);

    acc.str_val(&create.lf_version);
    acc.byte(CREATE_TAG);

    // Optional seed
    match &seed {
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
fn encode_exercise_node(
    acc: &mut Acc,
    exercise: &Exercise,
    node_id: &str,
    nodes_dict: &NodesDict<'_>,
    node_seeds: &[daml_transaction::NodeSeed],
) -> Result<()> {
    let seed = find_seed(node_id, node_seeds).context("Exercise node must have a seed")?;

    acc.str_val(&exercise.lf_version);
    acc.byte(EXERCISE_TAG);

    // Required seed — raw bytes, NOT optional-wrapped
    acc.raw(&seed);

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
    encode_node_ids(acc, &exercise.children, nodes_dict, node_seeds)?;

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
fn encode_rollback_node(
    acc: &mut Acc,
    rollback: &Rollback,
    nodes_dict: &NodesDict<'_>,
    node_seeds: &[daml_transaction::NodeSeed],
) -> Result<()> {
    acc.byte(ROLLBACK_TAG);
    encode_node_ids(acc, &rollback.children, nodes_dict, node_seeds)?;
    Ok(())
}

/// Encode a full node: NodeEncodingVersion + node-type-specific encoding, then SHA-256
fn hash_node(
    daml_node: &daml_transaction::Node,
    nodes_dict: &NodesDict<'_>,
    node_seeds: &[daml_transaction::NodeSeed],
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
            encode_create_node(&mut acc, create, &daml_node.node_id, node_seeds)?;
        }
        NodeType::Exercise(exercise) => {
            encode_exercise_node(
                &mut acc,
                exercise,
                &daml_node.node_id,
                nodes_dict,
                node_seeds,
            )?;
        }
        NodeType::Fetch(fetch) => {
            encode_fetch_node(&mut acc, fetch)?;
        }
        NodeType::Rollback(rollback) => {
            encode_rollback_node(&mut acc, rollback, nodes_dict, node_seeds)?;
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

/// Encode a list of node IDs as repeated hashed nodes
fn encode_node_ids(
    acc: &mut Acc,
    node_ids: &[String],
    nodes_dict: &NodesDict<'_>,
    node_seeds: &[daml_transaction::NodeSeed],
) -> Result<()> {
    acc.i32_val(node_ids.len() as i32);
    for node_id in node_ids {
        let daml_node = nodes_dict
            .get(node_id)
            .with_context(|| format!("Node '{}' not found in transaction", node_id))?;
        let h = hash_node(daml_node, nodes_dict, node_seeds)?;
        acc.hash_val(&h);
    }
    Ok(())
}

// ============================================================================
// Layer 1: Transaction hash
// ============================================================================

fn hash_transaction(tx: &proto_v2::interactive::DamlTransaction) -> Result<[u8; 32]> {
    let nodes_dict = build_nodes_dict(tx);

    let mut acc = Acc::new();
    acc.raw(&HASH_PURPOSE);
    acc.str_val(&tx.version);
    encode_node_ids(&mut acc, &tx.roots, &nodes_dict, &tx.node_seeds)?;

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
        encode_create_node(&mut create_acc, create, "unused", &[])?;
        let create_hash = create_acc.finish();
        debug!("  input_contract[{}] hash: {} (created_at={})",
            &create.contract_id[..create.contract_id.len().min(16)],
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
