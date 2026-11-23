//! Drills of the swap executor on a fake RFQ venue and a fake ledger: what fails its binding,
//! its scope, its packages or its price guard is never signed, and a signed swap never again.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use agent_control::policy::Allowlist;
use agent_control::{
    AnyReceipt, CommandKind, CommandScope, ControllerType, DEFAULT_LINKED_RULES, PackageId,
    ReceiptOutcome, UtcTime, controller_id_for,
};
use agent_logic::control_store::CommandRecord;
use anyhow::Context;
use base64::Engine;
use ed25519_dalek::SigningKey;
use orderbook_proto::orderbook::PendingCommand;
use orderbook_proto::rfqv2::prepare_atomic_transaction_request::Params as AtomicParams;
use orderbook_proto::rfqv2::{
    AtomicAcsContract, AtomicDisclosedContract, AtomicDvpSettleParams, AtomicFeeSpec,
    AtomicMessageSignature, AtomicQuote, ExecuteAtomicTransactionRequest,
    PrepareAtomicTransactionRequest, PrepareAtomicTransactionResponse,
};
use prost::Message;
use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2;
use tx_verifier::{OperationExpectation, VerifyMode};
use v2::interactive::metadata::input_contract::Contract as InputContractOneof;
use v2::interactive::metadata::{InputContract, SubmitterInfo};
use v2::interactive::transaction::v1::{Create, Exercise, Node as V1Node, node::NodeType};
use v2::interactive::{DamlTransaction, Metadata, PreparedTransaction, daml_transaction};
use v2::value::Sum;
use v2::{Identifier, Record, RecordField, Value};

use super::*;
use crate::atomic_swap::DirectedSubmit;
use crate::control::commands::tests::{accepting, fixture, signed_command};
use crate::control::commands::{
    CODE_PREFIX, CommandRpc, DirectedLedger, Mode, run_execute, run_pending, settle_open,
};
use crate::control::prompt::Scripted;
use crate::control::test_support::{FakeRpc, TestAgent};
use crate::control::{ControlCommands, ControlOptions, RpcError};
use crate::holdings_cache::{CC_INSTRUMENT, CachedHolding, instrument_key};
use crate::ledger_client::{ATOMIC_EXECUTE_AMBIGUOUS, CommandSearch};

const SYNC: &str = "sync::1";
const VENUE: &str = "00fe01";
const LP_INPUT: &str = "00fe02";
const PKG: &str = fixture::PKG;
/// The link controller of the test chains signs every command here.
const CONTROLLER: u8 = 0x21;

fn lp() -> String {
    fixture::party("lp", 0xa1)
}

fn lp2() -> String {
    fixture::party("lptwo", 0xa2)
}

fn fees() -> String {
    fixture::party("fees", 0xf1)
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str_exact(s).unwrap()
}

fn amount(s: &str) -> Amount {
    Amount::parse(s).unwrap()
}

fn now() -> UtcTime {
    UtcTime::from_unix_secs(agent_logic::clock::now_secs_i64()).unwrap()
}

fn tok() -> InstrumentRef {
    InstrumentRef {
        admin: PartyId::parse(&fixture::registry()).unwrap(),
        id: InstrumentId::parse("TOK").unwrap(),
    }
}

fn cc() -> InstrumentRef {
    agent_control::canton_coin(&PartyId::parse(&fixture::dso()).unwrap()).unwrap()
}

fn market() -> ListedMarket {
    ListedMarket {
        market_id: "TOK-CC".into(),
        base: tok(),
        quote: cc(),
        base_symbol: "TOK".into(),
        quote_symbol: "CC".into(),
    }
}

fn fast() -> Pace {
    Pace {
        gap: Duration::ZERO,
        tail_secs: 120,
        max_accepts: 3,
        max_rounds: 3,
    }
}

fn order(sell: InstrumentRef, a: &str, buy: InstrumentRef, m: &str, fee: &str) -> SwapOrder {
    SwapOrder {
        sell,
        sell_amount: amount(a),
        buy,
        min_receive: amount(m),
        max_fee_cc: amount(fee),
    }
}

fn swap_op(o: &SwapOrder) -> CommandOp {
    CommandOp::Swap {
        sell: o.sell.clone(),
        sell_amount: o.sell_amount,
        buy: o.buy.clone(),
        min_receive: o.min_receive,
        max_fee_cc: o.max_fee_cc,
    }
}

fn info(id: &str, lp: &str, base: &str, quote: &str, fee: Option<&str>) -> AtomicQuoteInfo {
    AtomicQuoteInfo {
        quote_id: id.into(),
        lp_name: if lp == self::lp() {
            "lp-one".into()
        } else {
            "lp-two".into()
        },
        lp_party_id: lp.into(),
        quantity: base.into(),
        quote_quantity: quote.into(),
        settlement_fee: fee.map(|f| AtomicFeeSpec {
            receiver: fees(),
            instrument_admin: fixture::dso(),
            instrument_id: "Amulet".into(),
            amount: f.into(),
        }),
        ..Default::default()
    }
}

// ---- prepared settles, as a ledger-service may return them ----

#[derive(Clone, Debug, PartialEq)]
struct Holding {
    cid: String,
    owner: String,
    id: String,
    amount: String,
}

impl Holding {
    fn new(cid: &str, owner: &str, id: &str, amount: impl ToString) -> Self {
        Self {
            cid: cid.into(),
            owner: owner.into(),
            id: id.into(),
            amount: amount.to_string(),
        }
    }

    fn create(&self) -> Create {
        if self.id == "Amulet" {
            fixture::amulet(&self.cid, &self.owner, &self.amount)
        } else {
            fixture::holding(&self.cid, &self.owner, &self.id, &self.amount)
        }
    }

    fn template(&self) -> (&'static str, &'static str) {
        if self.id == "Amulet" {
            ("Splice.Amulet", "Amulet")
        } else {
            ("Utility.Registry.Holding.V0.Holding", "Holding")
        }
    }
}

/// One settle and every field a drill rewrites.
#[derive(Clone, Debug)]
struct Settle {
    agent: String,
    venue_lp: String,
    venue_listed: bool,
    command_id: String,
    quote_id: String,
    side: String,
    base_amount: String,
    quote_amount: String,
    valid_until: i64,
    user_inputs: Vec<Holding>,
    user_input_cids: Vec<String>,
    lp_inputs: Vec<Holding>,
    created: Vec<Holding>,
    /// (receiver, instrument id, amount); instruments are CC or the registry's.
    fees: Vec<(String, String, String)>,
    second_root: bool,
}

fn ident((module, entity): (&str, &str)) -> Identifier {
    Identifier {
        package_id: PKG.into(),
        module_name: module.into(),
        entity_name: entity.into(),
    }
}

fn val(sum: Sum) -> Value {
    Value { sum: Some(sum) }
}

fn rec(fields: Vec<(&str, Value)>) -> Value {
    val(Sum::Record(Record {
        record_id: None,
        fields: fields
            .into_iter()
            .map(|(label, value)| RecordField {
                label: label.into(),
                value: Some(value),
            })
            .collect(),
    }))
}

fn party_v(p: &str) -> Value {
    val(Sum::Party(p.into()))
}

fn text(t: &str) -> Value {
    val(Sum::Text(t.into()))
}

fn list(elements: Vec<Value>) -> Value {
    val(Sum::List(v2::List { elements }))
}

fn instrument_v(id: &str) -> Value {
    let admin = if id == "Amulet" {
        fixture::dso()
    } else {
        fixture::registry()
    };
    rec(vec![("admin", party_v(&admin)), ("id", text(id))])
}

fn node(id: usize, node_type: NodeType) -> daml_transaction::Node {
    daml_transaction::Node {
        node_id: id.to_string(),
        versioned_node: Some(daml_transaction::node::VersionedNode::V1(V1Node {
            node_type: Some(node_type),
        })),
    }
}

fn exercise(
    cid: &str,
    template: (&str, &str),
    choice: &str,
    acting: &str,
    consuming: bool,
    chosen: Value,
    children: Vec<String>,
) -> Exercise {
    Exercise {
        lf_version: "2.1".into(),
        contract_id: cid.into(),
        package_name: "pkg".into(),
        template_id: Some(ident(template)),
        acting_parties: vec![acting.to_string()],
        choice_id: choice.into(),
        chosen_value: Some(chosen),
        consuming,
        children,
        ..Default::default()
    }
}

fn venue_contract(lp: &str) -> Create {
    let arg = rec(vec![
        ("provider", party_v(&fixture::party("prov", 0xb1))),
        ("lp", party_v(lp)),
        ("pairName", text("TOK-CC")),
        ("baseInstrumentId", instrument_v("TOK")),
        ("quoteInstrumentId", instrument_v("Amulet")),
        ("quotePublicKey", text("k")),
    ]);
    Create {
        lf_version: "2.1".into(),
        contract_id: VENUE.into(),
        package_name: "pkg".into(),
        template_id: Some(ident(("AtomicDVP", "AtomicDVP"))),
        argument: Some(arg),
        signatories: vec![lp.to_string()],
        ..Default::default()
    }
}

fn encode(s: &Settle) -> Vec<u8> {
    let quote = rec(vec![
        ("quoteId", text(&s.quote_id)),
        ("ticketId", text("")),
        ("user", party_v(&s.agent)),
        (
            "side",
            val(Sum::Enum(v2::Enum {
                enum_id: None,
                constructor: s.side.clone(),
            })),
        ),
        ("baseAmount", val(Sum::Numeric(s.base_amount.clone()))),
        ("quoteAmount", val(Sum::Numeric(s.quote_amount.clone()))),
        (
            "createdAtMicros",
            val(Sum::Int64(s.valid_until.saturating_sub(60_000_000))),
        ),
        ("validUntilMicros", val(Sum::Int64(s.valid_until))),
    ]);
    let fee_list = s
        .fees
        .iter()
        .map(|(receiver, id, amount)| {
            rec(vec![
                ("receiver", party_v(receiver)),
                ("instrumentId", instrument_v(id)),
                ("amount", val(Sum::Numeric(amount.clone()))),
            ])
        })
        .collect();
    let cids = |cids: &[String]| {
        list(
            cids.iter()
                .map(|c| val(Sum::ContractId(c.clone())))
                .collect(),
        )
    };
    let lp_cids: Vec<String> = s.lp_inputs.iter().map(|h| h.cid.clone()).collect();
    let chosen = rec(vec![
        ("quote", quote),
        ("lpInputHoldingCids", cids(&lp_cids)),
        ("userInputHoldingCids", cids(&s.user_input_cids)),
        (
            "fees",
            val(Sum::Optional(Box::new(v2::Optional {
                value: Some(Box::new(list(fee_list))),
            }))),
        ),
    ]);
    let mut nodes = Vec::new();
    let mut next = 1usize;
    let mut children = Vec::new();
    for input in s.user_inputs.iter().chain(&s.lp_inputs) {
        let owner = input.owner.clone();
        nodes.push(node(
            next,
            NodeType::Exercise(exercise(
                &input.cid,
                input.template(),
                "Archive",
                &owner,
                true,
                rec(vec![]),
                vec![],
            )),
        ));
        children.push(next.to_string());
        next += 1;
    }
    for created in &s.created {
        nodes.push(node(next, NodeType::Create(created.create())));
        children.push(next.to_string());
        next += 1;
    }
    let root = exercise(
        VENUE,
        ("AtomicDVP", "AtomicDVP"),
        "AtomicDVP_Settle",
        &s.agent,
        false,
        chosen,
        children,
    );
    nodes.insert(0, node(0, NodeType::Exercise(root)));
    let mut roots = vec!["0".to_string()];
    if s.second_root {
        let extra = exercise(
            VENUE,
            ("AtomicDVP", "AtomicDVP"),
            "AtomicDVP_Settle",
            &s.agent,
            false,
            rec(vec![]),
            vec![],
        );
        nodes.push(node(next, NodeType::Exercise(extra)));
        roots.push(next.to_string());
    }
    let mut inputs: Vec<Create> = s
        .user_inputs
        .iter()
        .chain(&s.lp_inputs)
        .map(Holding::create)
        .collect();
    if s.venue_listed {
        inputs.push(venue_contract(&s.venue_lp));
    }
    let node_seeds = nodes
        .iter()
        .map(|n| daml_transaction::NodeSeed {
            node_id: n.node_id.parse().unwrap(),
            seed: vec![7u8; 32],
        })
        .collect();
    PreparedTransaction {
        transaction: Some(DamlTransaction {
            version: "2.1".into(),
            roots,
            nodes,
            node_seeds,
        }),
        metadata: Some(Metadata {
            submitter_info: Some(SubmitterInfo {
                act_as: vec![s.agent.clone()],
                command_id: s.command_id.clone(),
            }),
            input_contracts: inputs
                .into_iter()
                .map(|c| InputContract {
                    created_at: 1,
                    event_blob: vec![1, 2, 3],
                    contract: Some(InputContractOneof::V1(c)),
                })
                .collect(),
            ..Default::default()
        }),
    }
    .encode_to_vec()
}

fn sub(a: &str, b: &str) -> String {
    (dec(a) - dec(b)).normalize().to_string()
}

/// The settle an honest ledger-service prepares for `params`: the agent pays its leg (and
/// the fees, from its inputs or the proceeds), the LP pays its own, change goes back.
fn honest(
    agent: &str,
    funded: &BTreeMap<String, Holding>,
    params: &AtomicDvpSettleParams,
    command_id: &str,
) -> Settle {
    let q = params.quote.as_ref().unwrap();
    let (pay, pay_amount, get, get_amount) = if q.side == "Sell" {
        (
            params.base_instrument_id.clone(),
            q.base_amount.clone(),
            params.quote_instrument_id.clone(),
            q.quote_amount.clone(),
        )
    } else {
        (
            params.quote_instrument_id.clone(),
            q.quote_amount.clone(),
            params.base_instrument_id.clone(),
            q.base_amount.clone(),
        )
    };
    let user_inputs: Vec<Holding> = params
        .user_input_holding_cids
        .iter()
        .map(|c| funded[c].clone())
        .collect();
    let lp_total = (dec(&get_amount) + dec("1000")).to_string();
    let lp_input = Holding::new(LP_INPUT, &params.lp_party, &get, &lp_total);
    let mut created = vec![Holding::new("00e1", &params.lp_party, &pay, &pay_amount)];
    let fee_total = q
        .lp_fees
        .iter()
        .fold(Decimal::ZERO, |total, f| total + dec(&f.amount));
    let mut received = get_amount.clone();
    let mut change: BTreeMap<String, Decimal> = BTreeMap::new();
    for input in &user_inputs {
        *change.entry(input.id.clone()).or_default() += dec(&input.amount);
    }
    if let Some(left) = change.get_mut(&pay) {
        *left -= dec(&pay_amount);
    }
    if fee_total > Decimal::ZERO {
        match change.get_mut("Amulet") {
            Some(left) => *left -= fee_total,
            None => received = (dec(&get_amount) - fee_total).normalize().to_string(),
        }
    }
    created.push(Holding::new("00e2", agent, &get, &received));
    for (n, (id, left)) in change.iter().enumerate() {
        if *left > Decimal::ZERO {
            created.push(Holding::new(
                &format!("00e3{n:02}"),
                agent,
                id,
                left.normalize(),
            ));
        }
    }
    created.push(Holding::new(
        "00e4",
        &params.lp_party,
        &get,
        sub(&lp_total, &get_amount),
    ));
    let mut fee_legs = Vec::new();
    for (n, f) in q.lp_fees.iter().enumerate() {
        created.push(Holding::new(
            &format!("00e5{n:02}"),
            &f.receiver,
            &f.instrument_id,
            &f.amount,
        ));
        fee_legs.push((
            f.receiver.clone(),
            f.instrument_id.clone(),
            f.amount.clone(),
        ));
    }
    Settle {
        agent: agent.into(),
        venue_lp: params.lp_party.clone(),
        venue_listed: true,
        command_id: command_id.into(),
        quote_id: q.quote_id.clone(),
        side: q.side.clone(),
        base_amount: q.base_amount.clone(),
        quote_amount: q.quote_amount.clone(),
        valid_until: q.valid_until_micros,
        user_input_cids: params.user_input_holding_cids.clone(),
        user_inputs,
        lp_inputs: vec![lp_input],
        created,
        fees: fee_legs,
        second_root: false,
    }
}

// ---- the fakes ----

#[derive(Default)]
struct Counts {
    requests: AtomicU32,
    accepts: AtomicU32,
    prepares: AtomicU32,
    signatures: AtomicU32,
    scans: AtomicU32,
}

impl Counts {
    fn get(&self) -> (u32, u32, u32) {
        let n = |a: &AtomicU32| a.load(Ordering::SeqCst);
        (n(&self.accepts), n(&self.prepares), n(&self.signatures))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum End {
    Executed,
    Ambiguous,
    Duplicate,
}

type Alter = Arc<dyn Fn(&mut Settle) + Send + Sync>;

/// A ledger-service that prepares what `alter` makes of the honest settle, runs the checks
/// the agent's ledger client runs, and counts a signature only once the hook allowed it.
struct Submit {
    agent: String,
    platform: intent::PlatformParties,
    funded: BTreeMap<String, Holding>,
    alter: Alter,
    end: End,
    mismatch: bool,
    counts: Arc<Counts>,
    carried_command: Arc<AtomicU32>,
    /// The next prepare only fails with this.
    refuse_once: Option<String>,
}

#[async_trait::async_trait]
impl DirectedSubmit for Submit {
    async fn submit_directed(
        &mut self,
        req: PrepareAtomicTransactionRequest,
        expectation: &OperationExpectation,
        command: &Command,
        bind: crate::ledger_client::PreSign<'_>,
    ) -> Result<ExecuteAtomicTransactionResponse> {
        self.counts.prepares.fetch_add(1, Ordering::SeqCst);
        if let Some(text) = self.refuse_once.take() {
            bail!("PrepareAtomicTransaction failed: {text}");
        }
        if req
            .controller_command
            .as_ref()
            .is_some_and(|c| Command::decode(&c.canonical).ok().as_ref() == Some(command))
        {
            self.carried_command.fetch_add(1, Ordering::SeqCst);
        }
        let Some(AtomicParams::AtomicDvpSettle(params)) = req.params.as_ref() else {
            bail!("not a settle")
        };
        let mut settle = honest(
            &self.agent,
            &self.funded,
            params,
            &command.canton_command_id(),
        );
        (self.alter)(&mut settle);
        let bytes = encode(&settle);
        let hash = tx_verifier::hasher::try_compute_hash(&bytes, "V2")?;
        let server = if self.mismatch {
            [9u8; 32].to_vec()
        } else {
            hash.to_vec()
        };
        let server = base64::engine::general_purpose::STANDARD.encode(server);
        let verification = tx_verifier::verify_and_hash_with(
            &bytes,
            &server,
            "V2",
            expectation,
            VerifyMode { strict_all: true },
            false,
        )?;
        let facts =
            tx_verifier::facts::extract_facts(&bytes, &self.agent).map_err(|e| e.to_string());
        let request = OwnRequest::Atomic(req.params.as_ref());
        intent::check_before_signing_directed(
            &self.agent,
            &self.platform,
            expectation,
            request,
            &verification,
            &facts,
            command,
        )
        .map_err(|refusal| anyhow!(refusal))?;
        let canton_id = command.canton_command_id();
        bind(&PreSignContext {
            command_id: &canton_id,
            transaction_id: "tx-1",
            verification: &verification,
            facts: &facts,
            prepared_transaction: &bytes,
        })
        .context("refused before signing")?;
        self.counts.signatures.fetch_add(1, Ordering::SeqCst);
        match self.end {
            End::Executed => Ok(ExecuteAtomicTransactionResponse {
                success: true,
                update_id: "upd-swap-1".into(),
                ..Default::default()
            }),
            End::Ambiguous => Err(anyhow!(
                "{ATOMIC_EXECUTE_AMBIGUOUS}: execute failed with transport error (tx may have committed)"
            )),
            End::Duplicate => Err(anyhow!(
                "{ATOMIC_EXECUTE_AMBIGUOUS}: DUPLICATE_COMMAND (an earlier submission likely committed)"
            )),
        }
    }
}

/// The prepared bytes a binding saw, and the hash computed over them.
type Bound = (Vec<u8>, [u8; 32]);

/// The rpc's RFQ venue: rounds of quotes, signed envelopes for accepted ones.
struct Desk {
    agent: String,
    markets: Vec<ListedMarket>,
    references: VecDeque<Option<Reference>>,
    rounds: VecDeque<Vec<AtomicQuoteInfo>>,
    lps: Vec<(String, String)>,
    key: (String, String),
    forge: bool,
    refuse_accepts: bool,
    swapper: AtomicSwapper,
    submit: Submit,
    /// The agent's own atomic client, used instead of `submit` when set.
    real: Option<AtomicProviderClient>,
    /// Moves the rpc clock this far ahead once a quote is accepted.
    jump: Option<(Arc<AtomicI64>, i64)>,
    /// Moves the rpc clock this far ahead while quotes are requested.
    slow_quotes: Option<(Arc<AtomicI64>, i64)>,
    /// When each quote request came.
    asked_at: Vec<tokio::time::Instant>,
    /// The bytes and computed hash each binding saw.
    bound: Arc<Mutex<Vec<Bound>>>,
    direction: &'static str,
    offered: Vec<AtomicQuoteInfo>,
    requests: Vec<QuoteRequest>,
    /// The quote id the signed envelope carries instead of the accepted one.
    envelope_quote_id: Option<String>,
    /// Runs inside the binding, before the agent's own.
    before_bind: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Desk {
    fn envelope_for(&self, q: &AtomicQuoteInfo) -> AtomicQuoteEnvelope {
        let payload = serde_json::json!({
            "lp": q.lp_party_id,
            "provider": fixture::party("prov", 0xb1),
            "pairName": "TOK-CC",
            "baseInstrumentId": {"admin": fixture::registry(), "id": "TOK"},
            "quoteInstrumentId": {"admin": fixture::dso(), "id": "Amulet"},
            "quotePublicKey": self.key.0,
        });
        let at = agent_logic::clock::now_micros_i64();
        let mut env = AtomicQuoteEnvelope {
            version: atomic_quote::envelope::ENVELOPE_VERSION.to_string(),
            synchronizer_id: SYNC.into(),
            dvp: Some(AtomicAcsContract {
                contract_id: VENUE.into(),
                template_id: "#atomic-dvp-v2:AtomicDVP:AtomicDVP".into(),
                created_event_blob: "blob".into(),
                payload_json: payload.to_string(),
            }),
            quote: Some(AtomicQuote {
                quote_id: self
                    .envelope_quote_id
                    .clone()
                    .unwrap_or_else(|| q.quote_id.clone()),
                ticket_id: String::new(),
                user: self.agent.clone(),
                side: if self.direction == "sell" {
                    "Sell".into()
                } else {
                    "Buy".into()
                },
                base_amount: q.quantity.clone(),
                quote_amount: q.quote_quantity.clone(),
                created_at_micros: at - 1_000_000,
                valid_until_micros: at + 90_000_000,
                lp_fees: q.settlement_fee.iter().cloned().collect(),
            }),
            lp_input_holding_cids: vec![LP_INPUT.into()],
            disclosed: vec![
                AtomicDisclosedContract {
                    contract_id: VENUE.into(),
                    ..Default::default()
                },
                AtomicDisclosedContract {
                    contract_id: LP_INPUT.into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let plain = crate::atomic_swap::envelope_from_proto(&env).unwrap();
        env.canonical_message =
            atomic_quote::envelope::canonical_from_dvp(&plain.dvp.payload, &plain.quote).unwrap();
        let signer = if self.forge {
            atomic_quote::gen_keypair().unwrap().priv_scalar_hex
        } else {
            self.key.1.clone()
        };
        env.quote_signature = atomic_quote::sign_quote(&signer, &env.canonical_message).unwrap();
        env
    }
}

#[async_trait::async_trait]
impl SwapDesk for Desk {
    async fn markets(&mut self) -> Result<Vec<ListedMarket>> {
        Ok(self.markets.clone())
    }

    async fn reference(&mut self, _market_id: &str) -> Result<Option<Reference>> {
        let next = if self.references.len() > 1 {
            self.references.pop_front()
        } else {
            self.references.front().copied()
        };
        Ok(next.flatten())
    }

    async fn lp_names(&mut self, parties: &[PartyId]) -> Result<Vec<String>> {
        Ok(self
            .lps
            .iter()
            .filter(|(_, p)| parties.iter().any(|x| x.as_str() == p))
            .map(|(n, _)| n.clone())
            .collect())
    }

    async fn ensure_preapproval(&mut self, _plan: &SwapPlan) -> Result<()> {
        Ok(())
    }

    async fn request_quotes(&mut self, request: &QuoteRequest) -> Result<RequestQuotesV2Response> {
        self.submit.counts.requests.fetch_add(1, Ordering::SeqCst);
        self.asked_at.push(tokio::time::Instant::now());
        if let Some((ahead, secs)) = &self.slow_quotes {
            ahead.store(*secs, Ordering::SeqCst);
        }
        self.requests.push(request.clone());
        self.direction = request.direction;
        let quotes = if self.rounds.len() > 1 {
            self.rounds.pop_front().unwrap_or_default()
        } else {
            self.rounds.front().cloned().unwrap_or_default()
        };
        let names = &request.lp_names;
        let quotes: Vec<AtomicQuoteInfo> = quotes
            .into_iter()
            .filter(|q| names.is_empty() || names.contains(&q.lp_name))
            .collect();
        self.offered = quotes.clone();
        Ok(RequestQuotesV2Response {
            success: true,
            rfq_id: "rfq-1".into(),
            quotes,
            ..Default::default()
        })
    }

    async fn accept(&mut self, _rfq_id: &str, quote_id: &str) -> Result<AcceptQuoteAtomicResponse> {
        self.submit.counts.accepts.fetch_add(1, Ordering::SeqCst);
        let quote = self
            .offered
            .iter()
            .find(|q| q.quote_id == quote_id)
            .cloned()
            .context("an offered quote")?;
        if self.refuse_accepts {
            return Ok(AcceptQuoteAtomicResponse {
                success: false,
                ..Default::default()
            });
        }
        Ok(AcceptQuoteAtomicResponse {
            success: true,
            envelope: Some(self.envelope_for(&quote)),
            ..Default::default()
        })
    }

    async fn settle(
        &mut self,
        envelope: &AtomicQuoteEnvelope,
        accepted: &AtomicQuoteInfo,
        direction: FillDirection,
        directed: &DirectedSettle<'_>,
        before_submit: DirectedBefore<'_>,
        bind: DirectedBind<'_>,
    ) -> Result<DirectedOutcome> {
        if let Some((ahead, secs)) = &self.jump {
            ahead.store(*secs, Ordering::SeqCst);
        }
        let bound = Arc::clone(&self.bound);
        let before_bind = self.before_bind.clone();
        let mut seen = |round: &DirectedRound<'_>, pc: &PreSignContext<'_>| -> Result<()> {
            let saw = (
                pc.prepared_transaction.to_vec(),
                pc.verification.computed_hash,
            );
            agent_logic::sync::lock(&bound).push(saw);
            if let Some(before_bind) = &before_bind {
                before_bind();
            }
            bind(round, pc)
        };
        let swapper = &self.swapper;
        let submitter: &mut dyn DirectedSubmit = match self.real.as_mut() {
            Some(client) => client,
            None => &mut self.submit,
        };
        swapper
            .settle_envelope_directed(
                envelope,
                accepted,
                direction,
                directed,
                submitter,
                before_submit,
                &mut seen,
            )
            .await
    }

    fn remint(&mut self) {
        self.real = None;
    }
}

/// The ledger of the executor: the desk, and a scan that finds the command once told to.
struct Ledger {
    desk: Desk,
    found: Arc<Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl DirectedLedger for Ledger {
    async fn ledger_end(&mut self) -> Result<i64> {
        Ok(42)
    }

    async fn submit_directed(
        &mut self,
        _req: orderbook_proto::ledger::PrepareTransactionRequest,
        _expectation: &OperationExpectation,
        _command: &Command,
        _bind: crate::ledger_client::PreSign<'_>,
    ) -> Result<ExecuteTransactionResponse> {
        bail!("a swap never submits a v1 transaction")
    }

    async fn find_command(&mut self, _command_id: &str, _from_offset: i64) -> CommandSearch {
        self.desk.submit.counts.scans.fetch_add(1, Ordering::SeqCst);
        match agent_logic::sync::lock(&self.found).clone() {
            Some(update) => CommandSearch::Found(update),
            None => CommandSearch::NotFound,
        }
    }

    fn swaps(&mut self) -> Option<&mut dyn SwapDesk> {
        Some(&mut self.desk)
    }

    fn remint(&mut self) {
        self.desk.remint();
    }
}

/// The rpc as a command sees it: the receipts posted to it, and its clock.
#[derive(Default)]
struct Receipts {
    listed: Mutex<Vec<PendingCommand>>,
    posted: Mutex<Vec<(Vec<u8>, [u8; 64])>>,
    /// How far, in seconds, the rpc clock runs ahead of this one.
    ahead: Arc<AtomicI64>,
    syncs: AtomicU32,
}

impl Receipts {
    fn last(&self, party: &PartyId) -> AnyReceipt {
        let (bytes, signature) = agent_logic::sync::lock(&self.posted)
            .last()
            .cloned()
            .expect("a receipt was posted");
        AnyReceipt::verify(&bytes, &signature, party).unwrap()
    }

    fn count(&self) -> usize {
        agent_logic::sync::lock(&self.posted).len()
    }
}

#[async_trait::async_trait]
impl CommandRpc for Receipts {
    fn checked_now(&self) -> Result<UtcTime> {
        now()
            .checked_add_secs(self.ahead.load(Ordering::SeqCst))
            .context("the clock")
    }

    async fn pending_commands(&self) -> Result<Vec<PendingCommand>, RpcError> {
        Ok(agent_logic::sync::lock(&self.listed).clone())
    }

    async fn post_receipt(&self, receipt: Vec<u8>, signature: [u8; 64]) -> Result<(), RpcError> {
        agent_logic::sync::lock(&self.posted).push((receipt, signature));
        Ok(())
    }

    async fn sync_log(&self, _ctx: &crate::control::ControlContext) -> Result<()> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A linked agent with a pinned fee party, the fixture's DSO and its synchronizer.
async fn agent(seed: u8, tag: &str, rules: Option<Rules>) -> (FakeRpc, TestAgent) {
    let rpc = FakeRpc::start().await;
    let mut agent = TestAgent::new([seed; 32], &rpc, tag);
    agent.config.dso_party = fixture::dso();
    agent.config.synchronizer_id = SYNC.into();
    agent.config.party_fee = Some(fees());
    agent.config.control.swap_max_deviation_bps = 100;
    match rules {
        Some(rules) => {
            let (chain, document) = accepting(agent.seed, &agent.party, rules);
            agent.linked_chain(&rpc, chain, &[document]).await;
        }
        None => agent.linked(&rpc, 1).await,
    }
    (rpc, agent)
}

/// Funds the agent with 100 TOK and 500 CC, blob-ready.
fn funding(agent: &TestAgent) -> BTreeMap<String, Holding> {
    let party = agent.party.to_string();
    BTreeMap::from([
        (
            "00ab01".to_string(),
            Holding::new("00ab01", &party, "TOK", "100"),
        ),
        (
            "00cc01".to_string(),
            Holding::new("00cc01", &party, "Amulet", "500"),
        ),
    ])
}

async fn desk(
    agent: &TestAgent,
    rounds: Vec<Vec<AtomicQuoteInfo>>,
    references: Vec<Option<Reference>>,
    alter: Alter,
    end: End,
) -> (Ledger, Arc<Counts>) {
    let mut config = agent.config.clone();
    config.connection_timeout_secs = 1;
    config.request_timeout_secs = 1;
    let cache = HoldingsCache::new(false);
    let funded = funding(agent);
    let holdings = funded
        .values()
        .map(|h| CachedHolding {
            contract_id: h.cid.clone(),
            template_id: if h.id == "Amulet" {
                TEMPLATE_AMULET.into()
            } else {
                TEMPLATE_HOLDING.into()
            },
            instrument: if h.id == "Amulet" {
                CC_INSTRUMENT.into()
            } else {
                instrument_key(&fixture::registry(), &h.id)
            },
            amount: dec(&h.amount),
            created_event_blob: Some("blob".into()),
            synchronizer_id: SYNC.into(),
            discovered_at: std::time::Instant::now(),
        })
        .collect();
    cache.add_created(holdings).await;
    let swapper = AtomicSwapper {
        config,
        cache,
        spend_reserve: true,
        verbose: false,
        dry_run: false,
        force: false,
        confirm: false,
        confirm_lock: agent_logic::confirm::new_confirm_lock(),
    };
    let counts = Arc::new(Counts::default());
    let kf = atomic_quote::gen_keypair().unwrap();
    let submit = Submit {
        agent: agent.party.to_string(),
        platform: intent::PlatformParties::from_config(&agent.config),
        funded,
        alter,
        end,
        mismatch: false,
        counts: Arc::clone(&counts),
        carried_command: Arc::new(AtomicU32::new(0)),
        refuse_once: None,
    };
    let desk = Desk {
        agent: agent.party.to_string(),
        markets: vec![market()],
        references: references.into(),
        rounds: rounds.into(),
        lps: vec![("lp-one".into(), lp()), ("lp-two".into(), lp2())],
        key: (kf.pub_spki_hex.clone(), kf.priv_scalar_hex.clone()),
        forge: false,
        refuse_accepts: false,
        swapper,
        submit,
        real: None,
        jump: None,
        slow_quotes: None,
        asked_at: Vec::new(),
        bound: Arc::default(),
        direction: "sell",
        offered: Vec::new(),
        requests: Vec::new(),
        envelope_quote_id: None,
        before_bind: None,
    };
    (
        Ledger {
            desk,
            found: Arc::new(Mutex::new(None)),
        },
        counts,
    )
}

fn as_is() -> Alter {
    Arc::new(|_| {})
}

/// A reference read now at `mid` CC per TOK.
fn mid(m: &str) -> Option<Reference> {
    Some(Reference {
        mid: dec(m),
        at: agent_logic::clock::now_secs_i64(),
    })
}

fn item(agent: &TestAgent, o: &SwapOrder) -> PendingCommand {
    signed_command(&agent.party, swap_op(o), now(), 600, agent.head().unwrap())
}

async fn record(agent: &TestAgent, id: &str) -> Option<CommandRecord> {
    let store = agent.store();
    let lock = store.lock().await.unwrap();
    store.commands(&lock).unwrap().get(id).cloned()
}

/// Runs one listed swap on the daemon's path with the fast pace.
async fn execute(
    ctx: &crate::control::ControlContext,
    receipts: &Receipts,
    ledger: &mut Ledger,
    item: &PendingCommand,
) -> Handled {
    let mut exec = Executor {
        ctx,
        rpc: receipts,
        ledger,
    };
    match super::super::admit(&mut exec, item, Mode::Automatic)
        .await
        .unwrap()
    {
        super::super::Admission::Done(handled) => handled,
        super::super::Admission::Ready(ready) => run(&mut exec, *ready, fast()).await.unwrap(),
    }
}

fn failed(failure: CommandFailure) -> (u16, &'static str) {
    (failure.code(), failure.code_name())
}

fn code_of(handled: &Handled) -> (u16, &'static str) {
    match handled {
        Handled::Refused { code, name, .. } | Handled::Failed { code, name, .. } => (*code, *name),
        other => panic!("expected a refusal or a failure, got {other:?}"),
    }
}

// ---- the plan, the filter and the guard ----

#[test]
fn the_market_names_the_orientation_and_the_request_carries_exact_text() {
    let markets = vec![market()];
    let sell_tok = order(tok(), "12.5", cc(), "100", "0");
    let plan = plan_for(&markets, &sell_tok).unwrap();
    assert_eq!((plan.sells_base, plan.buy_symbol.as_str()), (true, "CC"));
    let request = request_of(&plan, &sell_tok, &[]);
    assert_eq!(
        (
            request.direction,
            request.quantity.as_str(),
            request.quote_quantity
        ),
        ("sell", "12.5", None)
    );
    let sell_cc = order(cc(), "0.0000000001", tok(), "1", "0");
    let plan = plan_for(&markets, &sell_cc).unwrap();
    assert_eq!((plan.sells_base, plan.buy_symbol.as_str()), (false, "TOK"));
    let request = request_of(&plan, &sell_cc, &["lp-one".into()]);
    assert_eq!((request.direction, request.quantity.as_str()), ("buy", ""));
    assert_eq!(
        (request.quote_quantity.as_deref(), request.lp_names),
        (Some("0.0000000001"), vec!["lp-one".to_string()])
    );
    // No market trades another pair.
    let other = InstrumentRef {
        admin: tok().admin,
        id: InstrumentId::parse("TOK2").unwrap(),
    };
    assert_eq!(plan_for(&markets, &order(other, "1", cc(), "1", "0")), None);
}

#[test]
fn the_reference_is_the_mid_with_its_timestamp() {
    let at = Some(prost_types::Timestamp {
        seconds: 1_700_000_000,
        nanos: 0,
    });
    let price = |bid, ask, last| GetPriceResponse {
        bid,
        ask,
        last,
        timestamp: at,
        ..Default::default()
    };
    assert_eq!(
        reference_of(&price(Some(0.25), Some(0.75), 9.0)),
        Some(Reference {
            mid: dec("0.5"),
            at: 1_700_000_000
        })
    );
    assert_eq!(
        reference_of(&price(None, Some(0.75), 0.25)).map(|r| r.mid),
        Some(dec("0.25"))
    );
    assert_eq!(reference_of(&price(Some(0.0), Some(0.3), 0.0)), None);
    assert_eq!(
        reference_of(&GetPriceResponse {
            last: 0.25,
            ..Default::default()
        }),
        None,
        "no timestamp, no reference"
    );
}

fn guard_of<'a>(
    o: &'a SwapOrder,
    cc: &'a InstrumentRef,
    sells_base: bool,
    reference: Option<Reference>,
) -> PriceGuard<'a> {
    PriceGuard {
        order: o,
        sells_base,
        cc,
        max_bps: 100,
        reference,
    }
}

#[test]
fn the_guard_measures_the_adverse_deviation_with_the_cc_fee() {
    let at = 1_000;
    let cc_ref = cc();
    let sell_tok = order(tok(), "10", cc(), "1", "1");
    let guard = |o, sells_base, reference| guard_of(o, &cc_ref, sells_base, reference);
    let r = Some(Reference { mid: dec("2"), at });
    let selling = guard(&sell_tok, true, r);
    // 19.8 CC for 10 TOK is 100 bps under the mid; 19.79 is past it; more than the mid is no deviation.
    assert_eq!(
        selling.check(at, dec("10"), dec("19.8"), Decimal::ZERO),
        Ok(())
    );
    assert_eq!(
        selling.check(at, dec("10"), dec("19.79"), Decimal::ZERO),
        Err(CommandFailure::PriceDeviation)
    );
    assert_eq!(
        selling.check(at, dec("10"), dec("30"), Decimal::ZERO),
        Ok(())
    );
    // Buying CC, the fee comes off what is received.
    assert_eq!(
        selling.check(at, dec("10"), dec("20"), dec("0.3")),
        Err(CommandFailure::PriceDeviation)
    );
    // Selling CC for TOK, the fee adds to what is given.
    let sell_cc = order(cc(), "20", tok(), "1", "1");
    let buying = guard(&sell_cc, false, r);
    assert_eq!(
        buying.check(at, dec("20"), dec("10"), Decimal::ZERO),
        Ok(())
    );
    assert_eq!(
        buying.check(at, dec("20"), dec("10"), dec("0.3")),
        Err(CommandFailure::PriceDeviation)
    );
    assert_eq!(
        buying.check(at, dec("20"), dec("9.9"), Decimal::ZERO),
        Err(CommandFailure::PriceDeviation)
    );
    // Missing, stale or bad references are 1410; 0 bps turns the guard off.
    assert_eq!(
        guard(&sell_tok, true, None).check(at, dec("10"), dec("20"), Decimal::ZERO),
        Err(CommandFailure::NoReferencePrice)
    );
    let stale = Some(Reference {
        mid: dec("2"),
        at: at - 61,
    });
    assert_eq!(
        guard(&sell_tok, true, stale).check(at, dec("10"), dec("20"), Decimal::ZERO),
        Err(CommandFailure::NoReferencePrice)
    );
    let off = PriceGuard {
        max_bps: 0,
        ..guard(&sell_tok, true, None)
    };
    assert_eq!(off.check(at, dec("10"), dec("1"), Decimal::ZERO), Ok(()));
}

#[test]
fn the_filter_takes_only_exact_capped_quotes_at_the_minimum_and_ranks_them() {
    let o = order(tok(), "10", cc(), "19", "0.5");
    let cc_ref = cc();
    let fee_party = PartyId::parse(&fees()).unwrap();
    let allowed = |p: &PartyId| p.as_str() == lp();
    let filter = Filter {
        guard: PriceGuard {
            order: &o,
            sells_base: true,
            cc: &cc_ref,
            max_bps: 0,
            reference: None,
        },
        fee_party: Some(&fee_party),
        now: 0,
        lp_allowed: &allowed,
    };
    let judged = |q: &AtomicQuoteInfo| filter.judge(q).map(|p| (p.net, p.fee));
    assert_eq!(
        judged(&info("q1", &lp(), "10", "20", Some("0.5"))),
        Ok((dec("19.5"), dec("0.5")))
    );
    assert_eq!(
        judged(&info("q2", &lp(), "10.0000000001", "20", None)),
        Err(Miss::SIZE)
    );
    assert_eq!(
        judged(&info("q3", &lp(), "10", "20", Some("0.6"))),
        Err(Miss::FEE)
    );
    assert_eq!(
        judged(&info("q4", &lp(), "10", "19.4", Some("0.5"))),
        Err(Miss::MINIMUM)
    );
    assert_eq!(
        judged(&info("q5", &lp2(), "10", "30", None)),
        Err(Miss::OTHER_LP)
    );
    assert_eq!(
        judged(&info("q6", &lp(), "1e1", "20", None)),
        Err(Miss::SIZE)
    );
    let mut to_lp = info("q7", &lp(), "10", "20", Some("0.1"));
    if let Some(fee) = to_lp.settlement_fee.as_mut() {
        fee.receiver = lp();
    }
    assert_eq!(
        judged(&to_lp),
        Err(Miss::FEE),
        "a fee to anyone but the fee party"
    );
    let mut in_tok = info("q8", &lp(), "10", "20", Some("0.1"));
    if let Some(fee) = in_tok.settlement_fee.as_mut() {
        fee.instrument_admin = fixture::registry();
        fee.instrument_id = "TOK".into();
    }
    assert_eq!(judged(&in_tok), Err(Miss::FEE), "a fee in anything but CC");
    // Without a fee party no fee passes; a zero cap takes fee-less quotes only.
    let no_pin = Filter {
        guard: filter.guard,
        fee_party: None,
        now: 0,
        lp_allowed: &allowed,
    };
    assert_eq!(
        no_pin
            .judge(&info("q9", &lp(), "10", "20", Some("0.1")))
            .map(|p| p.fee),
        Err(Miss::FEE)
    );
    let mut picks: Vec<Pick> = ["c", "a", "b"]
        .iter()
        .zip([("20", Some("0.5")), ("20", Some("0.1")), ("20.5", None)])
        .map(|(id, (q, f))| filter.judge(&info(id, &lp(), "10", q, f)).unwrap())
        .collect();
    rank(&mut picks);
    let ids: Vec<&str> = picks.iter().map(|p| p.quote.quote_id.as_str()).collect();
    assert_eq!(
        ids,
        ["b", "a", "c"],
        "best net, then the smaller fee, then the id"
    );
}

#[test]
fn the_furthest_miss_names_the_failure() {
    let mut seen = Seen::default();
    assert_eq!(seen.failure(), CommandFailure::NoAcceptableQuote);
    for miss in [Miss::DEVIATION, Miss::MINIMUM, Miss::SIZE, Miss::OTHER_LP] {
        seen.miss(miss);
    }
    assert_eq!(seen.failure(), CommandFailure::BelowMinimum);
    assert_eq!(
        miss_of("own holdings selection failed for x (need 1; insufficient)"),
        Miss::HOLDINGS
    );
    assert_eq!(
        miss_of("the signed window ends at Some(1) µs, after the command expires (0 µs)"),
        Miss::WINDOW
    );
    assert_eq!(
        miss_of("H14 pre-check failed: bad signature"),
        Miss::REJECTED
    );
}

// ---- whole swaps ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quote_sized_swap_of_cc_settles_once_with_a_v2_receipt() {
    let (_rpc, agent) = agent(0xc1, "swap-cc", None).await;
    let ctx = agent.context().await;
    // Sell 20 CC for at least 9.5 TOK, at most 0.5 CC fee; the LP signs the CC digits exactly.
    let o = order(cc(), "20", tok(), "9.5", "0.5");
    let quotes = vec![
        info("q-best", &lp(), "10", "20", Some("0.2")),
        info("q-worse", &lp(), "9.6", "20", None),
    ];
    let (mut ledger, counts) = desk(
        &agent,
        vec![quotes],
        vec![mid("2.03")],
        as_is(),
        End::Executed,
    )
    .await;
    let receipts = Receipts::default();
    let item = item(&agent, &o);
    let handled = execute(&ctx, &receipts, &mut ledger, &item).await;
    assert_eq!(
        handled,
        Handled::Executed {
            update_id: "upd-swap-1".into(),
            delivered: true
        }
    );
    assert_eq!(
        counts.get(),
        (1, 1, 1),
        "one accept, one prepare, one signature"
    );
    assert_eq!(
        ledger.desk.submit.carried_command.load(Ordering::SeqCst),
        1,
        "the request carried the command"
    );
    let request = ledger.desk.requests.first().cloned().unwrap();
    assert_eq!(
        (
            request.direction,
            request.quote_quantity.as_deref(),
            request.lp_names.len()
        ),
        ("buy", Some("20"), 0)
    );
    let AnyReceipt::V2(receipt) = receipts.last(&agent.party) else {
        panic!("an executed swap posts v2")
    };
    assert_eq!(receipt.receipt.outcome, ReceiptOutcome::Executed);
    assert_eq!(receipt.result.quote_id.as_str(), "q-best");
    assert_eq!(
        (
            receipt.result.sold,
            receipt.result.received,
            receipt.result.fee_cc
        ),
        (amount("20"), amount("10"), amount("0.2"))
    );
    assert_eq!(receipt.result.lp.as_str(), lp());
    let kept = record(&agent, &item.command_id).await.unwrap();
    assert_eq!(kept.status, CommandStatus::Executed);
    assert_eq!(
        kept.attempt
            .as_ref()
            .and_then(|a| a.swap.as_ref())
            .map(|s| s.inputs.clone()),
        Some(vec!["00cc01".to_string()])
    );
    // Listed again, it only reposts the same receipt.
    let mut exec = Executor {
        ctx: &ctx,
        rpc: &receipts,
        ledger: &mut ledger,
    };
    let again = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert_eq!(
        again,
        Handled::Settled {
            status: CommandStatus::Executed,
            delivered: true
        }
    );
    assert_eq!(counts.get(), (1, 1, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selling_the_base_settles_by_quantity() {
    let (_rpc, agent) = agent(0xc2, "swap-tok", None).await;
    let ctx = agent.context().await;
    let o = order(tok(), "12.5", cc(), "24", "0");
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    let receipts = Receipts::default();
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    assert_eq!(counts.get(), (1, 1, 1));
    let request = ledger.desk.requests.first().cloned().unwrap();
    assert_eq!(
        (
            request.direction,
            request.quantity.as_str(),
            request.quote_quantity
        ),
        ("sell", "12.5", None)
    );
    let AnyReceipt::V2(receipt) = receipts.last(&agent.party) else {
        panic!("v2")
    };
    assert_eq!(
        (
            receipt.result.sold,
            receipt.result.received,
            receipt.result.fee_cc
        ),
        (amount("12.5"), amount("25"), Amount::ZERO)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_rewrite_of_the_settle_is_refused_with_zero_signatures() {
    let (_rpc, agent) = agent(0xc3, "swap-drills", None).await;
    let ctx = agent.context().await;
    let me = agent.party.to_string();
    let content = failed(CommandFailure::ContentRefused);
    // The net under the minimum is the evaluator's command_mismatch (reason 5); the rest are content refusals.
    let mismatch = (5, "command_mismatch");
    let drills: Vec<(&str, Alter)> = vec![
        (
            "sold A+1e-10",
            Arc::new(|s: &mut Settle| s.created[0].amount = "12.5000000001".into()),
        ),
        (
            "a substituted instrument",
            Arc::new(|s: &mut Settle| s.created[0].id = "TOK2".into()),
        ),
        (
            "less than the minimum",
            Arc::new(|s: &mut Settle| s.created[1].amount = "23.9".into()),
        ),
        (
            "the inflow to another party",
            Arc::new(|s: &mut Settle| s.created[1].owner = fixture::eve()),
        ),
        (
            "an extra root",
            Arc::new(|s: &mut Settle| s.second_root = true),
        ),
        (
            "another command id",
            Arc::new(|s: &mut Settle| {
                s.command_id = "ctl-00000000-0000-7000-8000-000000000000".into()
            }),
        ),
        (
            "no command id",
            Arc::new(|s: &mut Settle| s.command_id = String::new()),
        ),
        (
            "the venue input left out",
            Arc::new(|s: &mut Settle| s.venue_listed = false),
        ),
        (
            "an inverted side",
            Arc::new(|s: &mut Settle| s.side = "Buy".into()),
        ),
        (
            "a fee leg to the LP",
            Arc::new(|s: &mut Settle| s.fees.push((lp(), "Amulet".into(), "0.1".into()))),
        ),
    ];
    let mut seen = Vec::new();
    for (what, alter) in drills {
        let expected = if what == "less than the minimum" {
            mismatch
        } else {
            content
        };
        let o = order(tok(), "12.5", cc(), "24", "0");
        let (mut ledger, counts) = desk(
            &agent,
            vec![vec![info("q1", &lp(), "12.5", "25", None)]],
            vec![mid("2")],
            alter,
            End::Executed,
        )
        .await;
        let receipts = Receipts::default();
        let item = item(&agent, &o);
        let handled = execute(&ctx, &receipts, &mut ledger, &item).await;
        let (_, prepares, signatures) = counts.get();
        let outcome = receipts.last(&agent.party).receipt().outcome;
        let status = record(&agent, &item.command_id).await.map(|r| r.status);
        let want = if expected == content {
            (ReceiptOutcome::Failed, Some(CommandStatus::Failed))
        } else {
            (ReceiptOutcome::Refused, Some(CommandStatus::Refused))
        };
        if (prepares, signatures, code_of(&handled), (outcome, status)) != (1, 0, expected, want) {
            seen.push(format!(
                "{what}: {prepares} prepared, {signatures} signed, {handled:?}, {outcome:?}"
            ));
        }
    }
    assert!(seen.is_empty(), "{seen:#?}");
    // A hash the agent did not compute is never signed either.
    let o = order(tok(), "12.5", cc(), "24", "0");
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    ledger.desk.submit.mismatch = true;
    let receipts = Receipts::default();
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!((code_of(&handled), counts.get().2), (content, 0));
    // An undirected settle never carries a directed command id.
    let params = AtomicDvpSettleParams {
        venue_cid: VENUE.into(),
        quote: Some(AtomicQuote {
            quote_id: "q1".into(),
            user: me.clone(),
            side: "Sell".into(),
            base_amount: "12.5".into(),
            quote_amount: "25".into(),
            valid_until_micros: 1,
            ..Default::default()
        }),
        lp_party: lp(),
        base_instrument_id: "TOK".into(),
        base_instrument_admin: fixture::registry(),
        quote_instrument_id: "Amulet".into(),
        quote_instrument_admin: fixture::dso(),
        lp_input_holding_cids: vec![LP_INPUT.into()],
        user_input_holding_cids: vec!["00ab01".into()],
        ..Default::default()
    };
    let bytes = encode(&honest(
        &me,
        &funding(&agent),
        &params,
        "ctl-01890a5d-ac96-774b-bcce-b302099a8057",
    ));
    let expectation = OperationExpectation::AtomicDvpSettle {
        user_party: me.clone(),
        venue_cid: VENUE.into(),
        template_id: "#atomic-dvp-v2:AtomicDVP:AtomicDVP".into(),
        quote_id: "q1".into(),
        ticket_id: String::new(),
        ticket_cid: None,
        side: "Sell".into(),
        base_amount: "12.5".into(),
        quote_amount: "25".into(),
        lp_party: lp(),
        base_instrument_id: "TOK".into(),
        base_instrument_admin: fixture::registry(),
        quote_instrument_id: "Amulet".into(),
        quote_instrument_admin: fixture::dso(),
        valid_until_micros: 1,
        lp_input_holding_cids: vec![LP_INPUT.into()],
        user_input_holding_cids: vec!["00ab01".into()],
    };
    let hash = base64::engine::general_purpose::STANDARD
        .encode(tx_verifier::hasher::try_compute_hash(&bytes, "V2").unwrap());
    let verification = tx_verifier::verify_and_hash_with(
        &bytes,
        &hash,
        "V2",
        &expectation,
        VerifyMode { strict_all: true },
        false,
    )
    .unwrap();
    let facts = tx_verifier::facts::extract_facts(&bytes, &me).map_err(|e| e.to_string());
    let platform = intent::PlatformParties::from_config(&agent.config);
    let undirected = intent::check_before_signing(
        &me,
        &platform,
        &expectation,
        OwnRequest::Atomic(None),
        &verification,
        &facts,
    );
    match undirected {
        Err(intent::ContentRefusal::BadRequest(text)) => {
            assert!(text.contains("reserved_command_id"), "{text}")
        }
        other => panic!("expected the directed id to refuse an undirected settle, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forged_inexact_or_short_quotes_get_no_signature() {
    let (_rpc, agent) = agent(0xc4, "swap-quotes", None).await;
    let ctx = agent.context().await;
    let o = order(tok(), "12.5", cc(), "24", "0");
    // A forged envelope fails H14 at every accept.
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    ledger.desk.forge = true;
    let receipts = Receipts::default();
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!(code_of(&handled), failed(CommandFailure::LpRejected));
    assert_eq!(counts.get(), (3, 0, 0), "three accepts, nothing prepared");
    // A quote for another amount, or under the minimum, is never accepted.
    for (quote, failure) in [
        (
            info("q2", &lp(), "12.4999999999", "25", None),
            CommandFailure::InexactSize,
        ),
        (
            info("q3", &lp(), "12.5", "23.99", None),
            CommandFailure::BelowMinimum,
        ),
    ] {
        // A mid the short quote meets exactly, so only its size or its minimum refuses it.
        let (mut ledger, counts) = desk(
            &agent,
            vec![vec![quote]],
            vec![mid("1.9192")],
            as_is(),
            End::Executed,
        )
        .await;
        let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
        assert_eq!(code_of(&handled), failed(failure));
        assert_eq!(counts.get(), (0, 0, 0));
        assert_eq!(
            receipts.last(&agent.party).receipt().outcome,
            ReceiptOutcome::Failed
        );
    }
    // An LP that refuses its accept ends as lp_rejected.
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q4", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    ledger.desk.refuse_accepts = true;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!(
        (code_of(&handled), counts.get()),
        (failed(CommandFailure::LpRejected), (3, 0, 0))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_price_guard_drops_deviating_quotes_first_and_checks_again_at_signing() {
    let (_rpc, agent) = agent(0xc5, "swap-guard", None).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    // Selling 20 CC: q-fee pays more TOK, but with its 3 CC fee it costs 2.19 CC per TOK against a mid of 2.
    let o = order(cc(), "20", tok(), "9", "5");
    let quotes = vec![
        info("q-fee", &lp(), "10.5", "20", Some("3")),
        info("q-fair", &lp(), "10", "20", None),
    ];
    let (mut ledger, counts) = desk(
        &agent,
        vec![quotes.clone()],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    let AnyReceipt::V2(receipt) = receipts.last(&agent.party) else {
        panic!("v2")
    };
    assert_eq!(receipt.result.quote_id.as_str(), "q-fair");
    assert_eq!(counts.get(), (1, 1, 1));
    // Only deviating quotes: 1411; no reference or a stale one: 1410; nothing accepted.
    let stale = Some(Reference {
        mid: dec("2"),
        at: agent_logic::clock::now_secs_i64() - 61,
    });
    for (references, failure) in [
        (vec![mid("1.5")], CommandFailure::PriceDeviation),
        (vec![None], CommandFailure::NoReferencePrice),
        (vec![stale], CommandFailure::NoReferencePrice),
    ] {
        let (mut ledger, counts) = desk(
            &agent,
            vec![quotes.clone()],
            references,
            as_is(),
            End::Executed,
        )
        .await;
        let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
        assert_eq!(
            (code_of(&handled), counts.get()),
            (failed(failure), (0, 0, 0))
        );
    }
    // The reference moves between the accept and the signature: nothing is signed.
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![quotes[1].clone()]],
        vec![
            mid("2"),
            mid("1.5"),
            mid("2"),
            mid("1.5"),
            mid("2"),
            mid("1.5"),
        ],
        as_is(),
        End::Executed,
    )
    .await;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!(code_of(&handled), failed(CommandFailure::PriceDeviation));
    assert_eq!(counts.get(), (3, 3, 0), "prepared each time, signed never");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zero_bound_turns_the_guard_off() {
    let (_rpc, mut agent) = agent(0xc6, "swap-noguard", None).await;
    agent.config.control.swap_max_deviation_bps = 0;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let o = order(tok(), "12.5", cc(), "1", "0");
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "2", None)]],
        vec![None],
        as_is(),
        End::Executed,
    )
    .await;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    assert_eq!(counts.get(), (1, 1, 1));
}

fn signer() -> agent_control::ControllerId {
    let key = SigningKey::from_bytes(&[CONTROLLER; 32]);
    controller_id_for(ControllerType::Ed25519, &key.verifying_key().to_bytes())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_scope_names_the_lps_asked_and_the_one_the_settle_may_pay() {
    let party_of = |p: String| PartyId::parse(&p).unwrap();
    let scope = CommandScope {
        controller: signer(),
        commands: vec![CommandKind::Swap],
        pairs: Vec::new(),
        lps: vec![party_of(lp())],
    };
    let rules = Rules {
        scopes: vec![scope],
        ..DEFAULT_LINKED_RULES
    };
    let (_rpc, agent) = agent(0xc7, "swap-scope", Some(rules)).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let o = order(tok(), "12.5", cc(), "24", "0");
    // The better quote of the other LP is never asked for, nor taken.
    let quotes = vec![
        info("q-other", &lp2(), "12.5", "30", None),
        info("q-mine", &lp(), "12.5", "25", None),
    ];
    let (mut ledger, counts) = desk(
        &agent,
        vec![quotes.clone()],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    assert_eq!(
        ledger.desk.requests.first().map(|r| r.lp_names.clone()),
        Some(vec!["lp-one".to_string()])
    );
    assert_eq!(counts.get(), (1, 1, 1));
    // A settle on another LP's venue is refused by the scope with 1112, unsigned.
    let other_venue: Alter = Arc::new(|s: &mut Settle| s.venue_lp = lp2());
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![quotes[1].clone()]],
        vec![mid("2")],
        other_venue,
        End::Executed,
    )
    .await;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!(
        (code_of(&handled), counts.get()),
        ((1112, "lp_not_allowed"), (1, 1, 0))
    );
    assert_eq!(
        receipts.last(&agent.party).receipt().outcome,
        ReceiptOutcome::Refused
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_package_outside_the_rules_refuses_the_swap_unsigned() {
    let rules = Rules {
        packages: Allowlist {
            any: false,
            items: vec![PackageId([1; 32])],
        },
        ..DEFAULT_LINKED_RULES
    };
    let (_rpc, agent) = agent(0xc8, "swap-packages", Some(rules)).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let o = order(tok(), "12.5", cc(), "24", "0");
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!(
        (code_of(&handled), counts.get()),
        ((19, "package_not_allowed"), (1, 1, 0))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signed_swap_is_only_reconciled_and_never_signed_again() {
    let (_rpc, agent) = agent(0xc9, "swap-recover", None).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    for end in [End::Ambiguous, End::Duplicate] {
        let o = order(tok(), "12.5", cc(), "24", "0");
        let (mut ledger, counts) = desk(
            &agent,
            vec![vec![info("q1", &lp(), "12.5", "25", None)]],
            vec![mid("2")],
            as_is(),
            end,
        )
        .await;
        let item = item(&agent, &o);
        let handled = execute(&ctx, &receipts, &mut ledger, &item).await;
        assert!(
            matches!(handled, Handled::Unsettled(_)),
            "{end:?}: {handled:?}"
        );
        assert_eq!(counts.get(), (1, 1, 1));
        let kept = record(&agent, &item.command_id).await.unwrap();
        assert!(
            kept.status == CommandStatus::Executing && kept.may_be_signed(),
            "{kept:?}"
        );
        // Listed again before it is found: reconciled, not signed.
        let posted = receipts.count();
        let mut exec = Executor {
            ctx: &ctx,
            rpc: &receipts,
            ledger: &mut ledger,
        };
        assert!(matches!(
            run_pending(&mut exec, &item, Mode::Automatic)
                .await
                .unwrap(),
            Handled::Unsettled(_)
        ));
        // Found on the ledger by its ctl- id: executed, with the v2 receipt of the quote it recorded.
        *agent_logic::sync::lock(&ledger.found) = Some("upd-found".into());
        let mut exec = Executor {
            ctx: &ctx,
            rpc: &receipts,
            ledger: &mut ledger,
        };
        let settled = settle_open(&mut exec, &Default::default()).await.unwrap();
        assert_eq!(
            settled.first().map(|(_, h)| h.clone()),
            Some(Handled::Executed {
                update_id: "upd-found".into(),
                delivered: true
            })
        );
        assert_eq!(counts.get(), (1, 1, 1), "{end:?}: one signature in total");
        assert_eq!(receipts.count(), posted + 1);
        let AnyReceipt::V2(receipt) = receipts.last(&agent.party) else {
            panic!("v2 after recovery")
        };
        assert_eq!(
            (
                receipt.receipt.update_id.as_ref().map(|t| t.as_str()),
                receipt.result.quote_id.as_str()
            ),
            (Some("upd-found"), "q1")
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_swap_taking_quotes_elsewhere_is_left_alone_until_it_expires() {
    let (_rpc, agent) = agent(0xca, "swap-elsewhere", None).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let o = order(tok(), "12.5", cc(), "24", "0");
    let item = item(&agent, &o);
    let at = now().unix_secs();
    let attempt = CommandAttempt {
        canton_command_id: format!("ctl-{}", item.command_id),
        ledger_offset: 40,
        started_at: at,
        signing: false,
        usage: Vec::new(),
        verdict: None,
        swap: None,
    };
    {
        let store = agent.store();
        let lock = store.lock().await.unwrap();
        store
            .update_commands(&lock, at, |book| {
                book.receive(&item.command_id, &item.command, at + 600, at)?;
                book.begin(&item.command_id, attempt.clone(), at)
            })
            .unwrap();
    }
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    let mut exec = Executor {
        ctx: &ctx,
        rpc: &receipts,
        ledger: &mut ledger,
    };
    assert!(matches!(
        run_pending(&mut exec, &item, Mode::Automatic)
            .await
            .unwrap(),
        Handled::NotYet(_)
    ));
    assert_eq!((counts.get(), receipts.count()), ((0, 0, 0), 0));
    // Past its expiry an unsigned attempt fails like any other.
    let expired = signed_command(&agent.party, swap_op(&o), now(), 600, agent.head().unwrap());
    {
        let store = agent.store();
        let lock = store.lock().await.unwrap();
        let attempt = CommandAttempt {
            canton_command_id: format!("ctl-{}", expired.command_id),
            ..attempt
        };
        store
            .update_commands(&lock, at, |book| {
                book.receive(&expired.command_id, &expired.command, at - 1, at)?;
                book.begin(&expired.command_id, attempt, at)
            })
            .unwrap();
    }
    let mut exec = Executor {
        ctx: &ctx,
        rpc: &receipts,
        ledger: &mut ledger,
    };
    let settled = settle_open(&mut exec, &Default::default()).await.unwrap();
    let ended = settled
        .iter()
        .find(|(id, _)| *id == expired.command_id)
        .map(|(_, h)| h.clone())
        .unwrap();
    assert_eq!(code_of(&ended), failed(CommandFailure::SubmitFailed));
}

#[tokio::test]
async fn dry_run_refuses_control_execute_before_anything_is_recorded() {
    let execute = ControlCommands::Execute {
        code: "CMD-019abc00-0000-7000-8000-000000000001".into(),
    };
    let options = ControlOptions {
        dry_run: true,
        ..Default::default()
    };
    let refused = crate::control::refuse_dry_run(&options, &execute)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("--dry-run"), "{refused}");
    assert!(crate::control::refuse_dry_run(&options, &ControlCommands::Commands).is_ok());
    assert!(crate::control::refuse_dry_run(&ControlOptions::default(), &execute).is_ok());
    // The whole command stops at once: no config, store or rpc is touched.
    let mut config = BaseConfig::test_minimal().unwrap();
    config.orderbook_grpc_url = "http://127.0.0.1:9".into();
    let options = ControlOptions {
        control_dir: Some(std::env::temp_dir().join("never-created-swap-dry-run")),
        dry_run: true,
        ..Default::default()
    };
    let err = crate::control::run_control(config, options, execute)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("--dry-run"), "{err}");
    assert!(
        !std::env::temp_dir()
            .join("never-created-swap-dry-run")
            .exists()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_execute_shows_the_swap_and_takes_its_typed_phrase() {
    let (_rpc, agent) = agent(0xcb, "swap-execute", None).await;
    let ctx = agent.context().await;
    let o = order(cc(), "20", tok(), "9.5", "0.5");
    let item = item(&agent, &o);
    let receipts = Receipts::default();
    *agent_logic::sync::lock(&receipts.listed) = vec![item.clone()];
    let code = format!("{CODE_PREFIX}{}", item.command_id);
    let quotes = vec![info("q1", &lp(), "10", "20", None)];
    // A wrong phrase signs nothing and leaves the command received.
    let (mut ledger, counts) = desk(
        &agent,
        vec![quotes.clone()],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    let mut out = Vec::new();
    let refused = run_execute(
        &ctx,
        &receipts,
        &mut ledger,
        &Scripted::new(["swap 20 TOK"]),
        &mut out,
        &code,
    )
    .await;
    assert!(refused.is_err());
    assert_eq!((counts.get(), receipts.count()), ((0, 0, 0), 0));
    let shown = String::from_utf8(out).unwrap();
    assert!(
        shown.contains("Sell:       20 Amulet")
            && shown.contains("At least:   9.5 TOK")
            && shown.contains("Max fee:    0.5 CC"),
        "{shown}"
    );
    // The phrase names the sold amount and asset.
    let mut out = Vec::new();
    let handled = run_execute(
        &ctx,
        &receipts,
        &mut ledger,
        &Scripted::new(["swap 20 CC"]),
        &mut out,
        &code,
    )
    .await
    .unwrap();
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    assert_eq!(counts.get(), (1, 1, 1));
    assert!(matches!(receipts.last(&agent.party), AnyReceipt::V2(_)));
}

// ---- the agent's own atomic client, on a ledger-service over the wire ----

/// The key the wire ledger-service signs its answers with.
const WIRE_SEED: [u8; 32] = [0x5e; 32];

fn wire_signature(canonical: &[u8]) -> AtomicMessageSignature {
    let sig = message_signing::sign_canonical(&WIRE_SEED, canonical);
    AtomicMessageSignature {
        signature: sig.signature_b64,
        public_key: sig.public_key_b64url,
        signing_scheme: sig.signing_scheme,
    }
}

/// A ledger-service over the wire: it prepares what `alter` makes of the honest settle of
/// each request it receives, and counts the executes, which carry the signature.
#[derive(Clone)]
struct Wire {
    agent: String,
    funded: BTreeMap<String, Holding>,
    alter: Alter,
    mismatch: bool,
    served: Arc<Mutex<Vec<Vec<u8>>>>,
    executes: Arc<AtomicU32>,
}

impl Wire {
    fn prepare(
        &self,
        req: PrepareAtomicTransactionRequest,
    ) -> Result<PrepareAtomicTransactionResponse, tonic::Status> {
        let Some(AtomicParams::AtomicDvpSettle(params)) = req.params.as_ref() else {
            return Err(tonic::Status::invalid_argument("not a settle"));
        };
        let command = req
            .controller_command
            .as_ref()
            .and_then(|c| Command::decode(&c.canonical).ok())
            .ok_or_else(|| tonic::Status::invalid_argument("no controller command"))?;
        let mut settle = honest(
            &self.agent,
            &self.funded,
            params,
            &command.canton_command_id(),
        );
        (self.alter)(&mut settle);
        let bytes = encode(&settle);
        agent_logic::sync::lock(&self.served).push(bytes.clone());
        let computed = tx_verifier::hasher::try_compute_hash(&bytes, "V2")
            .map_err(|e| tonic::Status::internal(e.to_string()))?;
        let hash = base64::engine::general_purpose::STANDARD.encode(if self.mismatch {
            [9u8; 32]
        } else {
            computed
        });
        let (txid, version, fees) = ("tx-wire-1", "V2", "[]");
        let canonical = message_signing::canonical_prepare_response(
            txid,
            &hash,
            &settle.command_id,
            &bytes,
            version,
        );
        let fees_canonical =
            message_signing::canonical_tx_fees_authorization(&self.agent, txid, fees);
        Ok(PrepareAtomicTransactionResponse {
            transaction_id: txid.into(),
            prepared_transaction_hash: hash,
            command_id: settle.command_id.clone(),
            prepared_transaction: bytes,
            hashing_scheme_version: version.into(),
            fees_json: fees.into(),
            response_signature: Some(wire_signature(&canonical)),
            fees_signature: Some(wire_signature(&fees_canonical)),
            ..Default::default()
        })
    }

    fn execute(
        &self,
        _req: ExecuteAtomicTransactionRequest,
    ) -> Result<ExecuteAtomicTransactionResponse, tonic::Status> {
        self.executes.fetch_add(1, Ordering::SeqCst);
        let update = "upd-wire-1";
        let canonical =
            message_signing::canonical_execute_response(true, update, None, None, None, None);
        Ok(ExecuteAtomicTransactionResponse {
            success: true,
            update_id: update.into(),
            response_signature: Some(wire_signature(&canonical)),
            ..Default::default()
        })
    }
}

type Answer<Q, R> = Arc<dyn Fn(Q) -> Result<R, tonic::Status> + Send + Sync>;

struct Unary<Q, R>(Answer<Q, R>);

impl<Q: Send + 'static, R: Send + 'static> tonic::server::UnaryService<Q> for Unary<Q, R> {
    type Response = R;
    type Future = tonic::codegen::BoxFuture<tonic::Response<R>, tonic::Status>;

    fn call(&mut self, request: tonic::Request<Q>) -> Self::Future {
        let reply = (self.0)(request.into_inner());
        Box::pin(async move { reply.map(tonic::Response::new) })
    }
}

impl tonic::server::NamedService for Wire {
    const NAME: &'static str = "silvana.rfqv2.v1.AtomicDvpProviderService";
}

impl<B> tonic::codegen::Service<tonic::codegen::http::Request<B>> for Wire
where
    B: tonic::codegen::Body + Send + 'static,
    B::Error: Into<tonic::codegen::StdError> + Send + 'static,
{
    type Response = tonic::codegen::http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: tonic::codegen::http::Request<B>) -> Self::Future {
        use crate::control::test_support::Prost;
        let wire = self.clone();
        let path = req.uri().path().to_string();
        Box::pin(async move {
            Ok(if path.ends_with("/ExecuteAtomicTransaction") {
                let answer: Answer<
                    ExecuteAtomicTransactionRequest,
                    ExecuteAtomicTransactionResponse,
                > = Arc::new(move |q| wire.execute(q));
                let codec = Prost::<
                    ExecuteAtomicTransactionResponse,
                    ExecuteAtomicTransactionRequest,
                >::default();
                tonic::server::Grpc::new(codec)
                    .unary(Unary(answer), req)
                    .await
            } else {
                let answer: Answer<
                    PrepareAtomicTransactionRequest,
                    PrepareAtomicTransactionResponse,
                > = Arc::new(move |q| {
                    if path.ends_with("/PrepareAtomicTransaction") {
                        wire.prepare(q)
                    } else {
                        Err(tonic::Status::unimplemented("not in the fake"))
                    }
                });
                let codec = Prost::<
                    PrepareAtomicTransactionResponse,
                    PrepareAtomicTransactionRequest,
                >::default();
                tonic::server::Grpc::new(codec)
                    .unary(Unary(answer), req)
                    .await
            })
        })
    }
}

/// A rewrite, a wrong hash or not, and the answer, receipt and bindings it ends with.
type WireDrill = (
    &'static str,
    Alter,
    bool,
    (u16, &'static str),
    ReceiptOutcome,
    usize,
);

/// Serves a wire ledger-service for `agent` and sets the desk to settle through the agent's
/// own atomic client on it.
#[allow(clippy::disallowed_methods)]
async fn over_the_wire(
    ledger: &mut Ledger,
    agent: &TestAgent,
    alter: Alter,
    mismatch: bool,
) -> (Wire, tokio::task::JoinHandle<()>) {
    let wire = Wire {
        agent: agent.party.to_string(),
        funded: funding(agent),
        alter,
        mismatch,
        served: Arc::default(),
        executes: Arc::new(AtomicU32::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let service = wire.clone();
    let server = tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(incoming)
            .await;
    });
    let channel = tonic::transport::Channel::from_shared(url)
        .unwrap()
        .connect_lazy();
    let server_key = SigningKey::from_bytes(&WIRE_SEED)
        .verifying_key()
        .to_bytes();
    let client = AtomicProviderClient::for_tests(
        channel,
        agent.party.as_str(),
        &agent.config.private_key,
        &server_key,
    )
    .unwrap();
    ledger.desk.real = Some(client);
    (wire, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_agents_atomic_client_signs_a_directed_settle_once_and_only_after_its_binding() {
    let (_rpc, agent) = agent(0xcc, "swap-wire", None).await;
    let ctx = agent.context().await;
    let content = failed(CommandFailure::ContentRefused);
    let o = order(tok(), "12.5", cc(), "24", "0");
    let quotes = || vec![vec![info("q1", &lp(), "12.5", "25", None)]];
    // The honest settle: the binding sees the bytes it hashed, then one signature executes.
    let (mut ledger, _) = desk(&agent, quotes(), vec![mid("2")], as_is(), End::Executed).await;
    let (wire, server) = over_the_wire(&mut ledger, &agent, as_is(), false).await;
    let receipts = Receipts::default();
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    server.abort();
    assert_eq!(
        handled,
        Handled::Executed {
            update_id: "upd-wire-1".into(),
            delivered: true
        }
    );
    assert_eq!(wire.executes.load(Ordering::SeqCst), 1);
    let served = agent_logic::sync::lock(&wire.served).clone();
    let bound = agent_logic::sync::lock(&ledger.desk.bound).clone();
    assert_eq!(served.len(), 1);
    assert_eq!(bound.len(), 1, "the binding ran once");
    let (bytes, hash) = bound.first().cloned().unwrap();
    assert_eq!(Some(&bytes), served.first());
    assert_eq!(
        hash,
        tx_verifier::hasher::try_compute_hash(&bytes, "V2").unwrap()
    );
    let AnyReceipt::V2(receipt) = receipts.last(&agent.party) else {
        panic!("an executed swap posts v2")
    };
    assert_eq!(receipt.receipt.outcome, ReceiptOutcome::Executed);
    // What the binding refuses, the content check refuses, or a hash the agent did not
    // compute: each is prepared once and never executed.
    let drills: Vec<WireDrill> = vec![
        (
            "less than the minimum",
            Arc::new(|s: &mut Settle| s.created[1].amount = "23.9".into()),
            false,
            (5, "command_mismatch"),
            ReceiptOutcome::Refused,
            1,
        ),
        (
            "another command id",
            Arc::new(|s: &mut Settle| {
                s.command_id = "ctl-00000000-0000-7000-8000-000000000000".into()
            }),
            false,
            content,
            ReceiptOutcome::Failed,
            0,
        ),
        (
            "a hash it did not compute",
            as_is(),
            true,
            content,
            ReceiptOutcome::Failed,
            0,
        ),
    ];
    let mut seen = Vec::new();
    for (what, alter, mismatch, code, outcome, binds) in drills {
        let (mut ledger, _) = desk(&agent, quotes(), vec![mid("2")], as_is(), End::Executed).await;
        let (wire, server) = over_the_wire(&mut ledger, &agent, alter, mismatch).await;
        let receipts = Receipts::default();
        let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
        server.abort();
        let prepared = agent_logic::sync::lock(&wire.served).len();
        let executes = wire.executes.load(Ordering::SeqCst);
        let bound = agent_logic::sync::lock(&ledger.desk.bound).len();
        let posted = receipts.last(&agent.party).receipt().outcome;
        let got = (prepared, executes, bound, code_of(&handled), posted);
        if got != (1, 0, binds, code, outcome) {
            seen.push(format!("{what}: {got:?}"));
        }
    }
    assert!(seen.is_empty(), "{seen:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowing_that_falls_due_before_the_signature_refuses_the_swap_unsigned() {
    let rpc = FakeRpc::start().await;
    let mut agent = TestAgent::new([0xcd; 32], &rpc, "swap-narrowed");
    agent.config.dso_party = fixture::dso();
    agent.config.synchronizer_id = SYNC.into();
    agent.config.party_fee = Some(fees());
    // The price guard is off, so only the rules in force at signing can refuse.
    agent.config.control.swap_max_deviation_bps = 0;
    let transfers_only = Rules {
        scopes: vec![CommandScope {
            controller: signer(),
            commands: vec![CommandKind::TransferCc],
            pairs: Vec::new(),
            lps: Vec::new(),
        }],
        ..DEFAULT_LINKED_RULES
    };
    let due = now().checked_add_secs(300).unwrap();
    let (chain, documents) = crate::control::commands::tests::accepting_then(
        agent.seed,
        &agent.party,
        DEFAULT_LINKED_RULES,
        transfers_only,
        due,
    );
    agent.linked_chain(&rpc, chain, &documents).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let o = order(tok(), "12.5", cc(), "24", "0");
    let item = item(&agent, &o);
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    // Quoted and accepted before the narrowing; it falls due before the signature.
    ledger.desk.jump = Some((Arc::clone(&receipts.ahead), 400));
    let handled = execute(&ctx, &receipts, &mut ledger, &item).await;
    assert_eq!(code_of(&handled), (1108, "command_out_of_scope"));
    assert_eq!(
        counts.get(),
        (1, 1, 0),
        "accepted and prepared, never signed"
    );
    assert_eq!(
        receipts.last(&agent.party).receipt().outcome,
        ReceiptOutcome::Refused
    );
    assert_eq!(
        record(&agent, &item.command_id).await.map(|r| r.status),
        Some(CommandStatus::Refused)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_4103_at_the_settle_syncs_the_log_and_the_swap_runs_once_more() {
    let (_rpc, agent) = agent(0xce, "swap-repair", None).await;
    let ctx = agent.context().await;
    let o = order(tok(), "12.5", cc(), "24", "0");
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    ledger.desk.submit.refuse_once = Some("request refused (code 4103)".into());
    let receipts = Receipts::default();
    let item = item(&agent, &o);
    let mut exec = Executor {
        ctx: &ctx,
        rpc: &receipts,
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    assert_eq!(
        counts.get(),
        (2, 2, 1),
        "two accepts and prepares, one signature"
    );
    assert_eq!(receipts.syncs.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reference_gone_stale_while_the_quotes_arrive_takes_no_quote() {
    let (_rpc, agent) = agent(0xcf, "swap-stale", None).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let o = order(tok(), "12.5", cc(), "24", "0");
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    // The reference is fresh when read; the quotes arrive after it went stale.
    let late = REFERENCE_MAX_AGE_SECS + 40;
    ledger.desk.slow_quotes = Some((Arc::clone(&receipts.ahead), late));
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!(counts.get(), (0, 0, 0), "nothing accepted: {handled:?}");
    assert_eq!(
        receipts.last(&agent.party).receipt().outcome,
        ReceiptOutcome::Failed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rounds_keep_their_gap_and_stop_at_the_accept_limit() {
    // The production pace: at least 10 s between two requests, at most 5 accepts.
    assert_eq!(
        (PACE.gap, PACE.max_accepts, PACE.tail_secs),
        (Duration::from_secs(10), 5, 120)
    );
    let (_rpc, agent) = agent(0xd0, "swap-pace", None).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let o = order(tok(), "12.5", cc(), "24", "0");
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    ledger.desk.refuse_accepts = true;
    let pace = Pace {
        gap: Duration::from_millis(150),
        tail_secs: 120,
        max_accepts: 2,
        max_rounds: 10,
    };
    let item = item(&agent, &o);
    let mut exec = Executor {
        ctx: &ctx,
        rpc: &receipts,
        ledger: &mut ledger,
    };
    let handled = match super::super::admit(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap()
    {
        super::super::Admission::Done(handled) => handled,
        super::super::Admission::Ready(ready) => run(&mut exec, *ready, pace).await.unwrap(),
    };
    assert_eq!(code_of(&handled), failed(CommandFailure::LpRejected));
    assert_eq!(counts.get(), (2, 0, 0), "the accepts stop at the limit");
    let asked = ledger.desk.asked_at.clone();
    assert_eq!(asked.len(), 2);
    assert!(asked[1] - asked[0] >= pace.gap, "{asked:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quote_id_no_receipt_can_carry_is_never_accepted_or_signed() {
    let (_rpc, agent) = agent(0xdf, "swap-quote-id", None).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let o = order(tok(), "12.5", cc(), "24", "0");
    // The better quote's id is no token: the other one settles.
    let quotes = vec![
        info("q/best", &lp(), "12.5", "30", None),
        info("q-fair", &lp(), "12.5", "25", None),
    ];
    let (mut ledger, counts) =
        desk(&agent, vec![quotes], vec![mid("2")], as_is(), End::Executed).await;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    let AnyReceipt::V2(receipt) = receipts.last(&agent.party) else {
        panic!("an executed swap posts v2")
    };
    assert_eq!(receipt.result.quote_id.as_str(), "q-fair");
    assert_eq!(counts.get(), (1, 1, 1));
    // Alone, such a quote is never accepted.
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q/1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!(
        (code_of(&handled), counts.get()),
        (failed(CommandFailure::NoAcceptableQuote), (0, 0, 0))
    );
    // An envelope that settles under such an id is never signed.
    let (mut ledger, counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    ledger.desk.envelope_quote_id = Some("q/1".into());
    let handled = execute(&ctx, &receipts, &mut ledger, &item(&agent, &o)).await;
    assert_eq!(
        (code_of(&handled), counts.get()),
        (failed(CommandFailure::SubmitFailed), (3, 0, 0)),
        "accepted, never prepared or signed"
    );
    assert_eq!(
        receipts.last(&agent.party).receipt().outcome,
        ReceiptOutcome::Failed
    );
}

/// Rewrites the quote id the attempt of command `id` recorded in commands.json.
fn rewrite_quote_id(commands: &std::path::Path, id: &str, quote_id: &str) {
    let mut book: serde_json::Value =
        serde_json::from_slice(&std::fs::read(commands).unwrap()).unwrap();
    let record = book["commands"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["command_id"] == id)
        .unwrap();
    record["attempt"]["swap"]["quote_id"] = quote_id.into();
    std::fs::write(commands, serde_json::to_vec_pretty(&book).unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_executed_swap_without_its_v2_result_stays_executing_and_never_gets_a_v1_receipt() {
    let (_rpc, agent) = agent(0xde, "swap-withheld", None).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let commands = agent
        .store()
        .dir()
        .join(agent_logic::control_store::COMMANDS_FILE);
    let o = order(tok(), "12.5", cc(), "24", "0");
    for end in [End::Executed, End::Ambiguous] {
        let item = item(&agent, &o);
        let (mut ledger, counts) = desk(
            &agent,
            vec![vec![info("q1", &lp(), "12.5", "25", None)]],
            vec![mid("2")],
            as_is(),
            end,
        )
        .await;
        // Past the before hook, the recorded quote no longer reads as one a receipt carries.
        let (path, id) = (commands.clone(), item.command_id.clone());
        ledger.desk.before_bind = Some(Arc::new(move || rewrite_quote_id(&path, &id, "q/1")));
        let posted = receipts.count();
        let handled = execute(&ctx, &receipts, &mut ledger, &item).await;
        assert!(
            matches!(handled, Handled::Unsettled(_)),
            "{end:?}: {handled:?}"
        );
        // Found on the ledger: still no receipt, v1 or v2, and nothing signed again.
        *agent_logic::sync::lock(&ledger.found) = Some("upd-found".into());
        let mut exec = Executor {
            ctx: &ctx,
            rpc: &receipts,
            ledger: &mut ledger,
        };
        let settled = settle_open(&mut exec, &Default::default()).await.unwrap();
        let open = settled
            .iter()
            .find(|(c, _)| *c == item.command_id)
            .map(|(_, h)| h.clone());
        assert!(
            matches!(open, Some(Handled::Unsettled(_))),
            "{end:?}: {open:?}"
        );
        assert_eq!(
            (receipts.count(), counts.get()),
            (posted, (1, 1, 1)),
            "{end:?}"
        );
        let kept = record(&agent, &item.command_id).await.unwrap();
        assert!(
            kept.status == CommandStatus::Executing
                && kept.may_be_signed()
                && kept.receipt.is_none(),
            "{end:?}: {kept:?}"
        );
        // Its uses count meanwhile.
        let spends = ctx
            .store
            .update_counters(now().unix_secs(), |b| Ok(b.spends().to_vec()))
            .await
            .unwrap();
        assert!(
            spends
                .iter()
                .any(|s| s.key.starts_with(&format!("command:{}:", item.command_id))),
            "{end:?}: {spends:?}"
        );
        // Once the recorded quote reads again, its v2 receipt is built from it.
        rewrite_quote_id(&commands, &item.command_id, "q1");
        let mut exec = Executor {
            ctx: &ctx,
            rpc: &receipts,
            ledger: &mut ledger,
        };
        let settled = settle_open(&mut exec, &Default::default()).await.unwrap();
        let done = settled
            .iter()
            .find(|(c, _)| *c == item.command_id)
            .map(|(_, h)| h.clone());
        assert_eq!(
            done,
            Some(Handled::Executed {
                update_id: "upd-found".into(),
                delivered: true
            }),
            "{end:?}"
        );
        let AnyReceipt::V2(receipt) = receipts.last(&agent.party) else {
            panic!("{end:?}: v2 once the result reads")
        };
        assert_eq!(receipt.result.quote_id.as_str(), "q1");
        assert_eq!(counts.get(), (1, 1, 1), "{end:?}: one signature in total");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withheld_swap_counts_its_uses_once_at_the_attempts_time() {
    let (_rpc, agent) = agent(0xe0, "swap-withheld-once", None).await;
    let ctx = agent.context().await;
    let receipts = Receipts::default();
    let commands = agent
        .store()
        .dir()
        .join(agent_logic::control_store::COMMANDS_FILE);
    let o = order(tok(), "12.5", cc(), "24", "0");
    let item = item(&agent, &o);
    let (mut ledger, _counts) = desk(
        &agent,
        vec![vec![info("q1", &lp(), "12.5", "25", None)]],
        vec![mid("2")],
        as_is(),
        End::Executed,
    )
    .await;
    let (path, id) = (commands.clone(), item.command_id.clone());
    ledger.desk.before_bind = Some(Arc::new(move || rewrite_quote_id(&path, &id, "q/1")));
    let handled = execute(&ctx, &receipts, &mut ledger, &item).await;
    assert!(matches!(handled, Handled::Unsettled(_)), "{handled:?}");
    let started = record(&agent, &item.command_id)
        .await
        .and_then(|r| r.attempt)
        .map(|a| a.started_at)
        .unwrap();
    let key = format!("command:{}:", item.command_id);
    let counted_at = |at: i64| {
        let (store, key) = (ctx.store.clone(), key.clone());
        async move {
            let spends = store
                .update_counters(at, |b| Ok(b.spends().to_vec()))
                .await
                .unwrap();
            spends
                .iter()
                .filter(|s| s.key.starts_with(&key))
                .map(|s| s.at)
                .collect::<Vec<_>>()
        }
    };
    let first = counted_at(now().unix_secs()).await;
    assert!(
        !first.is_empty() && first.iter().all(|at| *at == started),
        "{first:?} at {started}"
    );
    // A day later it is still withheld: reconciled again, its uses are not counted again.
    let later = 25 * 3600;
    receipts.ahead.store(later, Ordering::SeqCst);
    *agent_logic::sync::lock(&ledger.found) = Some("upd-found".into());
    let mut exec = Executor {
        ctx: &ctx,
        rpc: &receipts,
        ledger: &mut ledger,
    };
    let settled = settle_open(&mut exec, &Default::default()).await.unwrap();
    let open = settled
        .iter()
        .find(|(c, _)| *c == item.command_id)
        .map(|(_, h)| h.clone());
    assert!(matches!(open, Some(Handled::Unsettled(_))), "{open:?}");
    let aged = counted_at(now().unix_secs().saturating_add(later)).await;
    assert!(aged.is_empty(), "aged out, never counted again: {aged:?}");
}
