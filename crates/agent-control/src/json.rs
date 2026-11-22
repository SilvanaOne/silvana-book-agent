//! A camelCase JSON view of every document, for `--json` output and the vectors.
//! A view only: numbers are decimal strings and bytes keep their canonical text.

use serde::Serialize;

use crate::canon::values::encode_b64url;
use crate::command::{Command, CommandOp};
use crate::controller::{ControllerDescriptor, ControllerKey};
use crate::entry::{EntryBody, LogEntry, SignedEntry};
use crate::head::HeadClaim;
use crate::link::LinkDescriptor;
use crate::policy::{Allowlist, Limits, PolicyDocument, PolicyInstrument, Rules};
use crate::proof::{ControllerProof, ProofEnvelope};
use crate::read_session::{ReadRequest, ReadSessionRequest};
use crate::receipt::Receipt;
use crate::{
    SCHEME_COMMAND, SCHEME_CONTROL, SCHEME_HEAD, SCHEME_LINK, SCHEME_POLICY, SCHEME_PROOF,
    SCHEME_READ_REQUEST, SCHEME_READ_SESSION, SCHEME_RECEIPT,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerJson {
    pub controller_id: String,
    pub fingerprint: String,
    #[serde(rename = "type")]
    pub controller_type: String,
    pub label: String,
    pub capabilities: String,
    pub public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rp_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origins: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_uv: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_policy: Option<String>,
}

impl From<&ControllerDescriptor> for ControllerJson {
    fn from(c: &ControllerDescriptor) -> Self {
        let passkey = match c.key() {
            ControllerKey::WebAuthn(passkey) => Some(passkey),
            ControllerKey::Ed25519(_) => None,
        };
        Self {
            controller_id: c.id().to_string(),
            fingerprint: c.id().fingerprint(),
            controller_type: c.controller_type().as_str().to_owned(),
            label: c.label().to_string(),
            capabilities: c.capabilities().to_string(),
            public_key: hex::encode(c.key().key_bytes()),
            credential_id: passkey.map(|p| encode_b64url(p.credential_id())),
            rp_id: passkey.map(|p| p.rp_id().to_string()),
            origins: passkey.map(|p| p.origins().iter().map(ToString::to_string).collect()),
            require_uv: passkey.map(|_| true),
            backup_policy: passkey.map(|p| p.backup_policy().as_str().to_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofJson {
    #[serde(rename = "type")]
    pub proof_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authenticator_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_data_json: Option<String>,
    /// Hex for Ed25519, base64url DER for WebAuthn.
    pub signature: String,
}

impl From<&ControllerProof> for ProofJson {
    fn from(proof: &ControllerProof) -> Self {
        match proof {
            ControllerProof::Ed25519 { signature } => Self {
                proof_type: proof.proof_type().as_str().to_owned(),
                authenticator_data: None,
                client_data_json: None,
                signature: hex::encode(signature),
            },
            ControllerProof::WebAuthn(assertion) => Self {
                proof_type: proof.proof_type().as_str().to_owned(),
                authenticator_data: Some(encode_b64url(&assertion.authenticator_data)),
                client_data_json: Some(encode_b64url(&assertion.client_data_json)),
                signature: encode_b64url(&assertion.signature),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofEnvelopeJson {
    pub scheme: &'static str,
    pub controller_id: String,
    pub payload_sha256: String,
    pub proof: ProofJson,
}

impl From<&ProofEnvelope> for ProofEnvelopeJson {
    fn from(e: &ProofEnvelope) -> Self {
        Self {
            scheme: SCHEME_PROOF,
            controller_id: e.controller_id().to_string(),
            payload_sha256: e.payload_sha256().to_string(),
            proof: e.proof().into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkDescriptorJson {
    pub scheme: &'static str,
    pub network: String,
    pub purpose: String,
    pub owner_account: String,
    /// `None` for an invite.
    pub agent_party: Option<String>,
    pub nonce: String,
    pub expires_at: String,
    pub controllers: Vec<ControllerJson>,
}

impl From<&LinkDescriptor> for LinkDescriptorJson {
    fn from(d: &LinkDescriptor) -> Self {
        Self {
            scheme: SCHEME_LINK,
            network: d.network().to_string(),
            purpose: d.purpose().as_str().to_owned(),
            owner_account: d.owner_account().to_string(),
            agent_party: d.agent_party().map(ToString::to_string),
            nonce: d.nonce().to_string(),
            expires_at: d.expires_at().to_string(),
            controllers: d.controllers().iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub enum EntryBodyJson {
    Link {
        owner_account: String,
        invite: bool,
        nonce: String,
        expires_at: String,
        commitment: String,
        controllers: Vec<ControllerJson>,
    },
    AddController {
        owner_account: String,
        nonce: Option<String>,
        expires_at: Option<String>,
        commitment: Option<String>,
        controller: ControllerJson,
    },
    RemoveController {
        controller_id: String,
        forced: bool,
    },
    SetCapabilities {
        controller_id: String,
        capabilities: String,
        forced: bool,
    },
    AcceptPolicy {
        policy_hash: String,
        policy_version: String,
        signed_by: String,
        proof: ProofJson,
    },
}

impl From<&EntryBody> for EntryBodyJson {
    fn from(body: &EntryBody) -> Self {
        match body {
            EntryBody::Link(b) => Self::Link {
                owner_account: b.owner_account.to_string(),
                invite: b.invite,
                nonce: b.nonce.to_string(),
                expires_at: b.expires_at.to_string(),
                commitment: b.commitment.to_string(),
                controllers: b.controllers.iter().map(Into::into).collect(),
            },
            EntryBody::AddController(b) => Self::AddController {
                owner_account: b.owner_account.to_string(),
                nonce: b.descriptor.map(|d| d.nonce.to_string()),
                expires_at: b.descriptor.map(|d| d.expires_at.to_string()),
                commitment: b.descriptor.map(|d| d.commitment.to_string()),
                controller: (&b.controller).into(),
            },
            EntryBody::RemoveController(b) => Self::RemoveController {
                controller_id: b.controller_id.to_string(),
                forced: b.forced,
            },
            EntryBody::SetCapabilities(b) => Self::SetCapabilities {
                controller_id: b.controller_id.to_string(),
                capabilities: b.capabilities.to_string(),
                forced: b.forced,
            },
            EntryBody::AcceptPolicy(b) => Self::AcceptPolicy {
                policy_hash: b.policy_hash.to_string(),
                policy_version: b.policy_version.to_string(),
                signed_by: b.signed_by.to_string(),
                proof: (&b.proof).into(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntryJson {
    pub scheme: &'static str,
    pub network: String,
    pub agent_party: String,
    pub agent_key: String,
    pub seq: String,
    pub prev: String,
    pub created_at: String,
    pub kind: String,
    pub effective_at: String,
    pub body: EntryBodyJson,
}

impl From<&LogEntry> for LogEntryJson {
    fn from(e: &LogEntry) -> Self {
        Self {
            scheme: SCHEME_CONTROL,
            network: e.network().to_string(),
            agent_party: e.agent_party().to_string(),
            agent_key: hex::encode(e.agent_key()),
            seq: e.seq().to_string(),
            prev: e.prev().to_string(),
            created_at: e.created_at().to_string(),
            kind: e.kind().as_str().to_owned(),
            effective_at: e.effective_at().to_string(),
            body: e.body().into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedEntryJson {
    pub entry: LogEntryJson,
    pub entry_hash: String,
    pub signature: String,
}

impl From<&SignedEntry> for SignedEntryJson {
    fn from(s: &SignedEntry) -> Self {
        Self {
            entry: s.entry().into(),
            entry_hash: s.hash().to_string(),
            signature: hex::encode(s.signature()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstrumentJson {
    pub admin: String,
    pub id: String,
}

impl From<&PolicyInstrument> for InstrumentJson {
    fn from(i: &PolicyInstrument) -> Self {
        Self {
            admin: i.admin_str().to_owned(),
            id: i.id_str().to_owned(),
        }
    }
}

fn instrument_view(instrument: &PolicyInstrument) -> InstrumentJson {
    instrument.into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AllowlistJson<T> {
    pub any: bool,
    pub items: Vec<T>,
}

fn allowlist<T, J>(list: &Allowlist<T>, view: impl Fn(&T) -> J) -> AllowlistJson<J> {
    AllowlistJson {
        any: list.any,
        items: list.items.iter().map(view).collect(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LimitItemJson {
    pub admin: String,
    pub id: String,
    pub amount: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LimitsJson {
    pub unlisted: String,
    pub items: Vec<LimitItemJson>,
}

impl From<&Limits> for LimitsJson {
    fn from(l: &Limits) -> Self {
        Self {
            unlisted: l.unlisted.as_str().to_owned(),
            items: l
                .items
                .iter()
                .map(|item| LimitItemJson {
                    admin: item.instrument.admin_str().to_owned(),
                    id: item.instrument.id_str().to_owned(),
                    amount: item.amount.to_string(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DelaysJson {
    pub transfers_secs: String,
    pub trading_secs: String,
    pub other_secs: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceJson {
    pub enabled: bool,
    pub traffic_cc_per_day: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TradingJson {
    pub enabled: bool,
    pub lp_enabled: bool,
    pub markets: AllowlistJson<String>,
    pub instruments: AllowlistJson<InstrumentJson>,
    pub per_trade: LimitsJson,
    pub per_day: LimitsJson,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransfersJson {
    pub enabled: bool,
    pub directed_only: bool,
    pub recipients: AllowlistJson<String>,
    pub eth_destinations: AllowlistJson<String>,
    pub per_transfer: LimitsJson,
    pub per_day: LimitsJson,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocksJson {
    pub enabled: bool,
    pub per_lock: LimitsJson,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EnabledJson {
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeesJson {
    pub cc_per_consent: Option<String>,
    pub cc_per_day: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RulesJson {
    pub paused: bool,
    pub delay: DelaysJson,
    pub maintenance: MaintenanceJson,
    pub trading: TradingJson,
    pub transfers: TransfersJson,
    pub locks: LocksJson,
    pub usdcx: EnabledJson,
    pub fees: FeesJson,
}

fn amount_text(amount: Option<crate::canon::values::Amount>) -> Option<String> {
    amount.map(|a| a.to_string())
}

impl From<&Rules> for RulesJson {
    fn from(r: &Rules) -> Self {
        Self {
            paused: r.paused,
            delay: DelaysJson {
                transfers_secs: r.delays.transfers_secs.to_string(),
                trading_secs: r.delays.trading_secs.to_string(),
                other_secs: r.delays.other_secs.to_string(),
            },
            maintenance: MaintenanceJson {
                enabled: r.maintenance.enabled,
                traffic_cc_per_day: amount_text(r.maintenance.traffic_cc_per_day),
            },
            trading: TradingJson {
                enabled: r.trading.enabled,
                lp_enabled: r.trading.lp_enabled,
                markets: allowlist(&r.trading.markets, ToString::to_string),
                instruments: allowlist(&r.trading.instruments, instrument_view),
                per_trade: (&r.trading.per_trade).into(),
                per_day: (&r.trading.per_day).into(),
            },
            transfers: TransfersJson {
                enabled: r.transfers.enabled,
                directed_only: r.transfers.directed_only,
                recipients: allowlist(&r.transfers.recipients, ToString::to_string),
                eth_destinations: allowlist(&r.transfers.eth_destinations, ToString::to_string),
                per_transfer: (&r.transfers.per_transfer).into(),
                per_day: (&r.transfers.per_day).into(),
            },
            locks: LocksJson {
                enabled: r.locks.enabled,
                per_lock: (&r.locks.per_lock).into(),
            },
            usdcx: EnabledJson {
                enabled: r.usdcx.enabled,
            },
            fees: FeesJson {
                cc_per_consent: amount_text(r.fees.cc_per_consent),
                cc_per_day: amount_text(r.fees.cc_per_day),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyJson {
    pub scheme: &'static str,
    pub network: String,
    pub agent_party: String,
    pub version: String,
    pub prev_policy_hash: String,
    pub issued_at: String,
    pub expires_at: String,
    pub not_before: Option<String>,
    pub rules: RulesJson,
}

impl From<&PolicyDocument> for PolicyJson {
    fn from(p: &PolicyDocument) -> Self {
        Self {
            scheme: SCHEME_POLICY,
            network: p.network.to_string(),
            agent_party: p.agent_party.to_string(),
            version: p.version.to_string(),
            prev_policy_hash: p.prev_policy_hash.to_string(),
            issued_at: p.issued_at.to_string(),
            expires_at: p.expires_at.to_string(),
            not_before: p.not_before.map(|t| t.to_string()),
            rules: (&p.rules).into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "name",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CommandOpJson {
    TransferCc {
        receiver: String,
        amount: String,
        description: Option<String>,
        settlement_proposal_id: Option<String>,
    },
    TransferToken {
        receiver: String,
        instrument_admin: String,
        instrument_id: String,
        amount: String,
        reference: Option<String>,
    },
    UsdcxBurn {
        amount: String,
        destination: String,
    },
}

impl From<&CommandOp> for CommandOpJson {
    fn from(op: &CommandOp) -> Self {
        match op {
            CommandOp::TransferCc {
                receiver,
                amount,
                description,
                settlement_proposal_id,
            } => Self::TransferCc {
                receiver: receiver.to_string(),
                amount: amount.to_string(),
                description: description.as_ref().map(ToString::to_string),
                settlement_proposal_id: settlement_proposal_id.as_ref().map(ToString::to_string),
            },
            CommandOp::TransferToken {
                receiver,
                instrument,
                amount,
                reference,
            } => Self::TransferToken {
                receiver: receiver.to_string(),
                instrument_admin: instrument.admin.to_string(),
                instrument_id: instrument.id.to_string(),
                amount: amount.to_string(),
                reference: reference.as_ref().map(ToString::to_string),
            },
            CommandOp::UsdcxBurn {
                amount,
                destination,
            } => Self::UsdcxBurn {
                amount: amount.to_string(),
                destination: destination.to_string(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandJson {
    pub scheme: &'static str,
    pub network: String,
    pub agent_party: String,
    pub command_id: String,
    pub log_seq: String,
    pub log_head: String,
    pub issued_at: String,
    pub expires_at: String,
    pub op: CommandOpJson,
    pub summary: String,
}

impl From<&Command> for CommandJson {
    fn from(c: &Command) -> Self {
        Self {
            scheme: SCHEME_COMMAND,
            network: c.network.to_string(),
            agent_party: c.agent_party.to_string(),
            command_id: c.command_id.to_string(),
            log_seq: c.log_seq.to_string(),
            log_head: c.log_head.to_string(),
            issued_at: c.issued_at.to_string(),
            expires_at: c.expires_at.to_string(),
            op: (&c.op).into(),
            summary: c.summary.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadSessionJson {
    pub scheme: &'static str,
    pub network: String,
    pub controller_id: String,
    pub session_key: String,
    pub nonce: String,
    pub issued_at: String,
    pub expires_at: String,
    pub agents: Vec<String>,
}

impl From<&ReadSessionRequest> for ReadSessionJson {
    fn from(r: &ReadSessionRequest) -> Self {
        Self {
            scheme: SCHEME_READ_SESSION,
            network: r.network.to_string(),
            controller_id: r.controller_id.to_string(),
            session_key: hex::encode(r.session_key.spki()),
            nonce: r.nonce.to_string(),
            issued_at: r.issued_at.to_string(),
            expires_at: r.expires_at.to_string(),
            agents: r.agents.iter().map(ToString::to_string).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadRequestJson {
    pub scheme: &'static str,
    pub network: String,
    pub session_id: String,
    pub method: String,
    pub agent_party: String,
    pub range_offset: String,
    pub range_limit: String,
    pub issued_at_ms: String,
    pub nonce: String,
}

impl From<&ReadRequest> for ReadRequestJson {
    fn from(r: &ReadRequest) -> Self {
        Self {
            scheme: SCHEME_READ_REQUEST,
            network: r.network.to_string(),
            session_id: r.session_id.to_string(),
            method: r.method.to_string(),
            agent_party: r.agent_party.to_string(),
            range_offset: r.range_offset.to_string(),
            range_limit: r.range_limit.to_string(),
            issued_at_ms: r.issued_at_ms.to_string(),
            nonce: r.nonce.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptJson {
    pub scheme: &'static str,
    pub network: String,
    pub agent_party: String,
    pub agent_key: String,
    pub subject: String,
    pub subject_id: String,
    pub subject_hash: String,
    pub outcome: String,
    pub update_id: Option<String>,
    pub reason_code: Option<String>,
    pub code_source: Option<String>,
    pub log_seq: String,
    pub issued_at: String,
}

impl From<&Receipt> for ReceiptJson {
    fn from(r: &Receipt) -> Self {
        Self {
            scheme: SCHEME_RECEIPT,
            network: r.network.to_string(),
            agent_party: r.agent_party.to_string(),
            agent_key: hex::encode(r.agent_key),
            subject: r.subject.kind_str().to_owned(),
            subject_id: r.subject.id_string(),
            subject_hash: r.subject_hash.to_string(),
            outcome: r.outcome.as_str().to_owned(),
            update_id: r.update_id.as_ref().map(ToString::to_string),
            reason_code: r.reason_code.map(|code| code.to_string()),
            code_source: r.code_source.map(|source| source.as_str().to_owned()),
            log_seq: r.log_seq.to_string(),
            issued_at: r.issued_at.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadClaimJson {
    pub scheme: &'static str,
    pub network: String,
    pub agent_party: String,
    pub agent_key: String,
    pub seq: String,
    pub head: String,
    pub issued_at: String,
}

impl From<&HeadClaim> for HeadClaimJson {
    fn from(h: &HeadClaim) -> Self {
        let (seq, head) = h
            .head
            .map_or((0, [0u8; 32]), |head| (head.seq, head.hash.0));
        Self {
            scheme: SCHEME_HEAD,
            network: h.network.to_string(),
            agent_party: h.agent_party.to_string(),
            agent_key: hex::encode(h.agent_key),
            seq: seq.to_string(),
            head: hex::encode(head),
            issued_at: h.issued_at.to_string(),
        }
    }
}

/// Pretty JSON of a view, for `--json` output.
pub fn to_pretty_json<T: Serialize>(view: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::DEFAULT_LINKED_RULES;
    use crate::testkit::{self, Chain, at};
    use serde_json::{Value, json};

    fn value<T: Serialize>(view: &T) -> Value {
        serde_json::to_value(view).unwrap()
    }

    #[test]
    fn the_invite_and_its_link_entry_read_as_expected() {
        let invite = testkit::invite_descriptor();
        let view = value(&LinkDescriptorJson::from(&invite));
        assert_eq!(view["scheme"], "silvana.agent.link.v1");
        assert_eq!(view["ownerAccount"], "42");
        assert_eq!(view["agentParty"], Value::Null);
        assert_eq!(view["controllers"][0]["type"], "ed25519");
        assert_eq!(view["controllers"][0]["capabilities"], "read");
        assert_eq!(
            view["controllers"][0]["fingerprint"],
            "CC57 6A6F 27AD 7873 7510"
        );
        assert!(view["controllers"][0].get("rpId").is_none());
        assert_eq!(view["controllers"][1]["type"], "webauthn");
        assert_eq!(view["controllers"][1]["capabilities"], "policy,direct");
        assert_eq!(view["controllers"][1]["requireUv"], true);
        assert_eq!(
            view["controllers"][1]["origins"][0],
            "https://app.silvana.link"
        );
        let chain = Chain::linked();
        let entry = value(&SignedEntryJson::from(&chain.entries[0]));
        assert_eq!(entry["entry"]["kind"], "link");
        assert_eq!(entry["entry"]["seq"], "1");
        assert_eq!(entry["entry"]["body"]["invite"], true);
        assert_eq!(
            entry["entry"]["body"]["commitment"],
            "fbc6bc1c6bdd1ec04b08d0fe4e163048"
        );
        assert_eq!(
            entry["entryHash"],
            "9b06b3e8612987e6b36a1946b8d149b3f99356d966f02a14c755ddea8d00635f"
        );
    }

    #[test]
    fn policies_commands_and_receipts_have_camel_case_views() {
        let state = Chain::linked().state;
        let doc = testkit::policy_doc(&state, 1, at(0), DEFAULT_LINKED_RULES);
        let view = value(&PolicyJson::from(&doc));
        assert_eq!(view["rules"]["delay"]["transfersSecs"], "86400");
        assert_eq!(view["rules"]["transfers"]["directedOnly"], true);
        assert_eq!(
            view["rules"]["trading"]["perTrade"]["unlisted"],
            "unlimited"
        );
        assert_eq!(view["rules"]["fees"]["ccPerDay"], Value::Null);
        assert_eq!(view["notBefore"], Value::Null);
        let envelope = testkit::passkey_envelope(b"payload");
        let proof = value(&ProofEnvelopeJson::from(&envelope));
        assert_eq!(proof["proof"]["type"], "webauthn");
        assert!(proof["proof"]["clientDataJson"].is_string());
        let ed = value(&ProofJson::from(
            testkit::ed25519_envelope(0x21, b"p").proof(),
        ));
        assert_eq!(ed["signature"].as_str().unwrap().len(), 128);
        assert!(ed.get("authenticatorData").is_none());
        let op = value(&CommandOpJson::from(&CommandOp::UsdcxBurn {
            amount: crate::canon::values::Amount::parse("5").unwrap(),
            destination: crate::canon::values::EthAddress([0xab; 20]),
        }));
        assert_eq!(
            op,
            json!({"name": "usdcx_burn", "amount": "5", "destination": format!("0x{}", "ab".repeat(20))})
        );
        let claim = HeadClaim::for_state(&state, at(5));
        let head = value(&HeadClaimJson::from(&claim));
        assert_eq!(head["seq"], "1");
        assert!(to_pretty_json(&head).unwrap().contains("\"agentParty\""));
    }
}
