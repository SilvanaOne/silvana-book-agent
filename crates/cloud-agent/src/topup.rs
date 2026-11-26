//! Auto top-up of the off-chain prepaid traffic balance.
//!
//! When `MIN_PREPAID_TRAFFIC_BALANCE_CC` and `PREPAID_TRAFFIC_TOPUP_CC` are
//! set, the agent monitors its prepaid traffic balance and tops it up by
//! submitting a `PrepayTraffic` ledger op (CC transfer to PARTY_PREPAID_TRAFFIC)
//! whenever the balance drops below the configured minimum.
//!
//! Attachment points (wired by the caller — see `lib.rs`):
//! 1. After every successful `submit_transaction` (fire-and-forget).
//! 2. On a 10-minute background timer (catches idle drift).
//! 3. After each settlement step the backend runs, paid or refused.
//!
//! With a fee source attached it also tops up when the available balance
//! is below what in-flight settlements still hold in prepaid.
//!
//! All three share a re-entrancy guard so the topup itself (which submits a tx)
//! never recurses or stacks up under load. The runner owns its OWN
//! DAppProviderClient (separate JWT + gRPC connection from the main agent
//! client) so the topup's submit_transaction does NOT trigger the per-tx
//! hook a second time and never blocks the main client mutex.
//! A top-up walks `PREPAID_TRAFFIC_TOPUP_TOKENS` in order: CC is a `PrepayTraffic`;
//! a USD stable buys the same CC through a purchase intent paid by a direct transfer.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Result};
use rust_decimal::{Decimal, RoundingStrategy};
use tokio::sync::{mpsc, Mutex as TokioMutex, OwnedMutexGuard};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};
use tx_verifier::facts::{FlowKind, TxFacts};
use tx_verifier::OperationExpectation;

use agent_logic::clock;
use agent_logic::config::{BaseConfig, CC_FEE_TOKEN};
use agent_logic::liquidity::LiquidityManager;
use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Policy};
use agent_logic::sync;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, PrepareTransactionRequest, PrepayTrafficParams,
    TokenBalance, TransactionOperation, TransferCip56Params,
};
use orderbook_proto::orderbook::{AgentTrafficPurchase, CreateTrafficPurchaseResponse};

use crate::ledger_client::{PreSignContext, SubmitOptions};
use crate::traffic_purchase::{log_safe, PurchaseApi, PurchaseState, SHORT_SKIP};
use crate::{DAppProviderClient, PrepaidTrafficBalance};

/// Bound on one topup attempt. A submit still running then is left to finish
/// in the background and holds off further attempts until it does.
const TOPUP_BUDGET: Duration = Duration::from_secs(300);

/// Period of the background balance check.
const CHECK_INTERVAL: Duration = Duration::from_secs(600);

/// Quiet period of the runner's own checks after a submit ends or the wallet is short of CC.
const TOPUP_COOLDOWN: Duration = Duration::from_secs(90);

/// Capacity of the nudge channel; a full channel drops further nudges.
const NUDGE_SLOTS: usize = 16;

type HeldSince = Arc<std::sync::Mutex<Option<Instant>>>;

/// Decimals of a stable top-up amount, rounded up.
pub const STABLE_DECIMALS: u32 = 6;
/// Least credit an intent may quote, in percent of the CC asked for.
const MIN_CREDIT_PCT: u32 = 97;
/// Least time between two status reads of a payment in flight.
const STATUS_POLL_EVERY: Duration = Duration::from_secs(30);
/// The purchase lifetime (15 min) plus the server's apply give-up (30 min): a payment of
/// unknown outcome waits this long before its purchase status decides.
pub const UNKNOWN_OUTCOME_WAIT: Duration = Duration::from_secs(45 * 60);
/// A payment still waiting after this long is warned about, then again each time it passes.
const STILL_WAITING_WARN: Duration = Duration::from_secs(20 * 60);
/// The file in the agent folder that keeps the payment in flight across restarts.
pub const PAYMENT_FILE: &str = "traffic-topup-payment.json";
/// Largest payment file read.
const MAX_PAYMENT_FILE_BYTES: u64 = 16 * 1024;
/// Longest text field of a payment file.
const MAX_RECORD_FIELD_CHARS: usize = 512;

/// The traffic-fee party in force, read by each walk.
pub type PinSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// A pin source that always names `pin`.
pub fn fixed_pin(pin: &str) -> PinSource {
    let pin = pin.to_string();
    Arc::new(move || Some(pin.clone()))
}

/// The installed control pin, else the configured one; a blank pin is unset.
pub fn traffic_fee_pin(installed: Option<&str>, configured: Option<&str>) -> Option<String> {
    let named = |pin: &str| {
        let pin = pin.trim();
        (!pin.is_empty()).then(|| pin.to_string())
    };
    installed.and_then(named).or_else(|| configured.and_then(named))
}

/// A stable payment waiting for its credit, as [`PAYMENT_FILE`] keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentRecord {
    pub purchase_id: String,
    pub memo: String,
    pub token: String,
    pub token_amount: Decimal,
    /// The update the payment committed in; `None` while its outcome is unknown.
    pub update_id: Option<String>,
    /// When the payment started, in Unix milliseconds.
    pub started_at_ms: u64,
}

const RECORD_KEYS: [&str; 6] = ["purchase_id", "memo", "token", "token_amount", "update_id", "started_at_ms"];

impl PaymentRecord {
    fn to_json(&self) -> Value {
        json!({
            "purchase_id": self.purchase_id,
            "memo": self.memo,
            "token": self.token,
            "token_amount": self.token_amount.to_string(),
            "update_id": self.update_id,
            "started_at_ms": self.started_at_ms,
        })
    }

    fn from_json(value: &Value) -> Result<Self> {
        let fields = value.as_object().ok_or_else(|| anyhow!("not an object"))?;
        if let Some(key) = fields.keys().find(|key| !RECORD_KEYS.contains(&key.as_str())) {
            bail!("unknown field {}", log_safe(key));
        }
        let text = |key: &str| -> Result<String> {
            let value = fields.get(key).and_then(Value::as_str).ok_or_else(|| anyhow!("{key} is missing"))?;
            ensure!(
                !value.trim().is_empty() && value.trim() == value && value.chars().count() <= MAX_RECORD_FIELD_CHARS,
                "{key} is blank, padded or too long"
            );
            Ok(value.to_string())
        };
        let token_amount =
            Decimal::from_str_exact(&text("token_amount")?).map_err(|_| anyhow!("token_amount is not a decimal"))?;
        ensure!(token_amount > Decimal::ZERO, "token_amount is not positive");
        let update_id = match fields.get("update_id") {
            None | Some(Value::Null) => None,
            Some(_) => Some(text("update_id")?),
        };
        let started_at_ms =
            fields.get("started_at_ms").and_then(Value::as_u64).ok_or_else(|| anyhow!("started_at_ms is missing"))?;
        Ok(Self { purchase_id: text("purchase_id")?, memo: text("memo")?, token: text("token")?, token_amount, update_id, started_at_ms })
    }
}

/// The payment file's record; `None` when there is no file.
pub fn load_payment(path: &Path) -> Result<Option<PaymentRecord>> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!("cannot open {}: {e}", path.display()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PAYMENT_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?;
    ensure!(u64::try_from(bytes.len()).is_ok_and(|n| n <= MAX_PAYMENT_FILE_BYTES), "{} is too large", path.display());
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| anyhow!("{} is not JSON: {e}", path.display()))?;
    PaymentRecord::from_json(&value)
        .map(Some)
        .map_err(|e| anyhow!("{} is not a top-up payment record: {e:#}", path.display()))
}

/// Replaces the payment file with `record`, atomically and durably.
pub fn save_payment(path: &Path, record: &PaymentRecord) -> Result<()> {
    let value = record.to_json();
    // What could not be read back is never written.
    PaymentRecord::from_json(&value).map_err(|e| anyhow!("the payment record would not read back: {e:#}"))?;
    let bytes = serde_json::to_vec_pretty(&value)?;
    agent_logic::control_store::write_atomic(path, &bytes, 0o600)
}

/// Removes the payment file; a missing one is fine.
fn remove_payment_file(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(file = %path.display(), error = %e, "The top-up payment file was not removed"),
    }
}

/// When `started_at_ms` (wall clock) was on this process's clock; now if it cannot be placed.
fn since_of(started_at_ms: u64) -> Instant {
    let now = Instant::now();
    let elapsed = Duration::from_millis(clock::now_millis().saturating_sub(started_at_ms));
    now.checked_sub(elapsed).unwrap_or(now)
}

/// One entry of the top-up priority list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopupToken {
    /// Canton Coin, through `PrepayTraffic`.
    Cc,
    /// A USD stable bought through a purchase intent: its wire id and registry.
    Stable { token: String, wire_id: String, registry: String },
}

impl TopupToken {
    pub fn name(&self) -> &str {
        match self {
            TopupToken::Cc => CC_FEE_TOKEN,
            TopupToken::Stable { token, .. } => token,
        }
    }
}

/// Whether a top-up can move wallet CC, so wallet CC counts as fee capacity.
pub fn moves_wallet_cc(tokens: &[TopupToken]) -> bool {
    tokens.contains(&TopupToken::Cc)
}

/// The wire id and registry of a fee token in the agent's instrument registry.
pub fn resolve_fee_token(config: &BaseConfig, token: &str) -> Option<(String, String)> {
    let resolved = config.try_resolve_instrument(token).ok().or_else(|| {
        let r = config.resolve_instrument_or_wire(token);
        r.wire_id.zip(r.registry)
    });
    resolved.filter(|(wire, registry)| !wire.trim().is_empty() && !registry.trim().is_empty())
}

/// `PREPAID_TRAFFIC_TOPUP_TOKENS` resolved, in order; a stable the registry cannot place is left out.
pub fn topup_tokens(config: &BaseConfig) -> Vec<TopupToken> {
    let mut tokens = Vec::new();
    for token in &config.prepaid_traffic_topup_tokens {
        if token.eq_ignore_ascii_case(CC_FEE_TOKEN) {
            tokens.push(TopupToken::Cc);
            continue;
        }
        match resolve_fee_token(config, token) {
            Some((wire_id, registry)) => tokens.push(TopupToken::Stable { token: token.clone(), wire_id, registry }),
            None => warn!(token = %token, "Top-up token left out: the instrument registry does not list it"),
        }
    }
    tokens
}

/// A stable top-up amount: `topup_cc × rate`, rounded up to [`STABLE_DECIMALS`]; none unless positive.
pub fn stable_amount(topup_cc: Decimal, cc_usd_rate: Decimal) -> Option<Decimal> {
    let amount = topup_cc
        .checked_mul(cc_usd_rate)?
        .round_dp_with_strategy(STABLE_DECIMALS, RoundingStrategy::AwayFromZero);
    (amount > Decimal::ZERO).then(|| amount.normalize())
}

/// The unlocked amount of a balance; an older answer without it reads as total less locked.
pub fn unlocked_amount(balance: &TokenBalance) -> Option<Decimal> {
    if !balance.unlocked_amount.trim().is_empty() {
        return Decimal::from_str_exact(balance.unlocked_amount.trim()).ok();
    }
    let total = Decimal::from_str_exact(balance.total_amount.trim()).ok()?;
    let locked = match balance.locked_amount.trim() {
        "" => Decimal::ZERO,
        locked => Decimal::from_str_exact(locked).ok()?,
    };
    total.checked_sub(locked)
}

/// Unlocked Canton Coin.
pub fn unlocked_cc(balances: &[TokenBalance]) -> Decimal {
    balances.iter().find(|b| b.is_canton_coin).and_then(unlocked_amount).unwrap_or(Decimal::ZERO)
}

/// Unlocked holdings of one registry instrument.
pub fn unlocked_of(balances: &[TokenBalance], wire_id: &str, registry: &str) -> Decimal {
    balances
        .iter()
        .filter(|b| !b.is_canton_coin && b.instrument_id == wire_id && b.instrument_admin == registry)
        .filter_map(unlocked_amount)
        .fold(Decimal::ZERO, |sum, amount| sum.saturating_add(amount))
}

/// What a top-up needs from the ledger service.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait TopupLedger: Send + Sync {
    async fn prepaid_balance(&self) -> Result<PrepaidTrafficBalance>;
    async fn balances(&self) -> Result<Vec<TokenBalance>>;
    /// Submits a CC `PrepayTraffic`; the update id.
    async fn prepay(&self, request: PrepareTransactionRequest, expectation: OperationExpectation) -> Result<String>;
    /// Submits one payment, signing only what its check accepts; the update id.
    async fn pay(&self, payment: Payment) -> Result<String>;
}

/// [`TopupLedger`] over the runner's own client.
pub struct ClientLedger(pub DAppProviderClient);

#[async_trait::async_trait]
impl TopupLedger for ClientLedger {
    async fn prepaid_balance(&self) -> Result<PrepaidTrafficBalance> {
        self.0.clone().get_prepaid_traffic_balance().await
    }

    async fn balances(&self) -> Result<Vec<TokenBalance>> {
        self.0.clone().get_balances().await
    }

    async fn prepay(&self, request: PrepareTransactionRequest, expectation: OperationExpectation) -> Result<String> {
        let mut client = self.0.clone();
        let submitted = client.submit_transaction(request, &expectation, false, false, false).await?;
        Ok(submitted.update_id)
    }

    async fn pay(&self, payment: Payment) -> Result<String> {
        let Payment { request, expectation, check } = payment;
        let mut hook = move |ctx: &PreSignContext<'_>| check_payment(ctx.facts, &check);
        let opts = SubmitOptions { max_retries: 1, pre_sign: Some(&mut hook) };
        let mut client = self.0.clone();
        let submitted = client.submit_transaction_guarded(request, &expectation, false, false, false, opts).await?;
        Ok(submitted.update_id)
    }
}

/// A stable payment: the request, what the verifier expects, and what may be signed.
#[derive(Clone, Debug)]
pub struct Payment {
    pub request: PrepareTransactionRequest,
    pub expectation: OperationExpectation,
    pub check: PaymentCheck,
}

/// The one direct transfer a payment may sign.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentCheck {
    pub receiver: String,
    pub instrument_id: String,
    pub instrument_admin: String,
    pub amount: Decimal,
    pub memo: String,
}

/// Exactly one outflow, a holding to the receiver of the instrument and amount, with the memo.
pub fn check_payment(facts: &Result<TxFacts, String>, want: &PaymentCheck) -> Result<()> {
    let facts = facts.as_ref().map_err(|e| anyhow!("the prepared payment cannot be read: {e}"))?;
    let [flow] = facts.outflows.as_slice() else {
        bail!("the prepared payment moves {} outflows, not exactly one", facts.outflows.len());
    };
    if flow.kind != FlowKind::Holding {
        bail!("the prepared payment is a {:?}, not a direct holding transfer", flow.kind);
    }
    if flow.to != want.receiver {
        bail!("the prepared payment goes to another party than PARTY_TRAFFIC_FEE");
    }
    if flow.instrument_id != want.instrument_id || flow.instrument_admin != want.instrument_admin {
        bail!("the prepared payment moves another instrument");
    }
    if flow.amount != want.amount {
        bail!("the prepared payment moves {} instead of {}", flow.amount, want.amount);
    }
    if !facts.memos.contains(&want.memo) {
        bail!("the prepared payment does not carry the purchase memo");
    }
    Ok(())
}

/// What a purchase intent must name.
#[derive(Clone, Debug)]
pub struct IntentWant<'a> {
    pub recipient: &'a str,
    pub instrument_id: &'a str,
    pub instrument_admin: &'a str,
    pub token_amount: Decimal,
    pub topup_cc: Decimal,
}

/// A checked intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurchaseOrder {
    pub purchase_id: String,
    pub memo: String,
    pub credit_cc: Decimal,
}

/// The intent's purchase when it names the pinned recipient, the local instrument and the
/// amount asked, and credits at least 97% of the CC asked for; otherwise why not.
pub fn check_intent(intent: &CreateTrafficPurchaseResponse, want: &IntentWant<'_>) -> Result<PurchaseOrder, String> {
    let purchase = intent.purchase.as_ref().ok_or("the intent carries no purchase")?;
    if purchase.purchase_id.trim().is_empty() {
        return Err("the intent has no id".into());
    }
    if intent.recipient_party != want.recipient {
        return Err("the intent names another recipient than PARTY_TRAFFIC_FEE".into());
    }
    if intent.instrument_id != want.instrument_id || intent.instrument_admin != want.instrument_admin {
        return Err("the intent names another instrument than the local registry".into());
    }
    if !agent_control::is_traffic_purchase_memo(&intent.memo) {
        return Err("the intent's memo does not name a traffic purchase".into());
    }
    if Decimal::from_str_exact(purchase.token_amount.trim()).ok() != Some(want.token_amount) {
        return Err(format!("the intent asks another amount than {}", want.token_amount));
    }
    let credit_cc = Decimal::from_str_exact(purchase.credit_cc.trim()).map_err(|_| "the intent's credit is unreadable")?;
    let floor = want
        .topup_cc
        .checked_mul(Decimal::from(MIN_CREDIT_PCT))
        .and_then(|v| v.checked_div(Decimal::ONE_HUNDRED))
        .ok_or("the credit floor is out of range")?;
    if credit_cc < floor {
        return Err(format!("the intent credits {credit_cc} CC, under {MIN_CREDIT_PCT}% of {} CC", want.topup_cc));
    }
    Ok(PurchaseOrder { purchase_id: purchase.purchase_id.clone(), memo: intent.memo.clone(), credit_cc })
}

/// The direct transfer that pays `order`, with its memo as the reference.
pub fn payment_for(party: &str, want: &IntentWant<'_>, order: &PurchaseOrder) -> Payment {
    let amount = want.token_amount.to_string();
    Payment {
        request: PrepareTransactionRequest {
            operation: TransactionOperation::TransferCip56 as i32,
            params: Some(Params::TransferCip56(TransferCip56Params {
                instrument_id: want.instrument_id.to_string(),
                instrument_admin: want.instrument_admin.to_string(),
                receiver_party: want.recipient.to_string(),
                amount: amount.clone(),
                reference: Some(order.memo.clone()),
                input_holding_cids: Vec::new(),
                max_input_holdings: None,
                direct_only: true,
            })),
            request_signature: None,
            controller_command: None,
        },
        expectation: OperationExpectation::TransferCip56 {
            sender_party: party.to_string(),
            receiver_party: want.recipient.to_string(),
            instrument_id: want.instrument_id.to_string(),
            instrument_admin: want.instrument_admin.to_string(),
            amount,
            command_id: None,
            input_holding_cids: Vec::new(),
        },
        check: PaymentCheck {
            receiver: want.recipient.to_string(),
            instrument_id: want.instrument_id.to_string(),
            instrument_admin: want.instrument_admin.to_string(),
            amount: want.token_amount,
            memo: order.memo.clone(),
        },
    }
}

/// Why a stable gave no intent to pay.
enum Unbought {
    /// Refused or unanswered: the token is left out this long.
    Skip(Duration, String),
    /// A foreign or unfair intent, cancelled: the attempt holds off.
    HoldOff(String),
}

/// Creates an intent for `token` and checks it; a refused one is cancelled. A checked
/// intent's memo stays open until its purchase is released.
async fn buy_intent(api: &dyn PurchaseApi, token: &str, want: &IntentWant<'_>, deadline: Instant) -> Result<PurchaseOrder, Unbought> {
    let created = match tokio::time::timeout_at(deadline, api.create(token, &want.token_amount.to_string())).await {
        Err(_) => return Err(Unbought::Skip(SHORT_SKIP, "no answer from the purchase service".into())),
        Ok(Err(e)) => return Err(Unbought::Skip(e.skip_for(), e.to_string())),
        Ok(Ok(created)) => created,
    };
    match check_intent(&created, want) {
        Ok(order) if crate::control::intent::insert_open_traffic_purchase(&order.memo) => Ok(order),
        Ok(order) => {
            cancel_intent(api, &order.purchase_id, deadline).await;
            Err(Unbought::Skip(SHORT_SKIP, "the intent's memo cannot be tracked".into()))
        }
        Err(why) => {
            if let Some(purchase) = created.purchase.as_ref().filter(|p| !p.purchase_id.trim().is_empty()) {
                cancel_intent(api, &purchase.purchase_id, deadline).await;
            }
            Err(Unbought::HoldOff(why))
        }
    }
}

/// Best-effort cancel of an unpaid intent, so it stops counting toward the open cap.
async fn cancel_intent(api: &dyn PurchaseApi, purchase_id: &str, deadline: Instant) {
    match tokio::time::timeout_at(deadline, api.cancel(purchase_id)).await {
        Ok(Ok(_)) => debug!(purchase_id = %log_safe(purchase_id), "Unpaid traffic purchase cancelled"),
        Ok(Err(e)) => warn!(purchase_id = %log_safe(purchase_id), error = %e, "Unpaid traffic purchase not cancelled; it expires on its own"),
        Err(_) => warn!(purchase_id = %log_safe(purchase_id), "Unpaid traffic purchase cancel unanswered; it expires on its own"),
    }
}

/// A stable payment waiting for its credit.
#[derive(Clone, Debug)]
struct OpenPayment {
    record: PaymentRecord,
    /// When it started, on this process's clock.
    since: Instant,
    /// When its status was last read or it was paid; `None` after a restart.
    checked: Option<Instant>,
    /// When it was last warned about.
    warned: Option<Instant>,
}

type Paying = Arc<std::sync::Mutex<Option<OpenPayment>>>;

/// Forgets `open`, its memo and, while it is the one in flight, its file.
fn finish_payment(paying: &Paying, file: Option<&Path>, open: &OpenPayment) {
    let current = {
        let mut slot = sync::lock(paying);
        let current = slot.as_ref().is_some_and(|p| p.record.purchase_id == open.record.purchase_id);
        if current {
            *slot = None;
        }
        current
    };
    crate::control::intent::remove_open_traffic_purchase(&open.record.memo);
    if let Some(file) = file.filter(|_| current) {
        remove_payment_file(file);
    }
}

/// Records that the payment of `purchase_id` committed in `update_id`; it is never paid again.
fn mark_committed(paying: &Paying, file: Option<&Path>, purchase_id: &str, update_id: &str) {
    if update_id.trim().is_empty() {
        warn!(purchase_id = %log_safe(purchase_id), "Top-up payment submitted without an update id; its outcome stays unknown");
        return;
    }
    let record = {
        let mut slot = sync::lock(paying);
        let Some(open) = slot.as_mut().filter(|p| p.record.purchase_id == purchase_id) else {
            return;
        };
        open.record.update_id = Some(update_id.to_string());
        open.record.clone()
    };
    if let Some(Err(e)) = file.map(|file| save_payment(file, &record)) {
        warn!(error = %format!("{e:#}"), "The committed top-up payment is not on file; after a restart its outcome counts as unknown");
    }
}

/// What a stable top-up needs besides the ledger.
#[derive(Clone)]
pub struct StableTopups {
    /// The pinned traffic-fee party every intent must name, read by each walk.
    pub traffic_fee_party: PinSource,
    pub purchases: Arc<dyn PurchaseApi>,
    /// Source of the fresh CC/USD rate.
    pub rates: Arc<LiquidityManager>,
}

/// What one token's try did.
enum Tried {
    /// Submitted, in flight or held off: the attempt ends.
    Done,
    /// This token could not top up; the next one is tried.
    Next(OwnedMutexGuard<()>, String),
}

/// Shared state for the auto-topup hook.
///
/// Owns its own `DAppProviderClient` so the topup's `submit_transaction`
/// does not recurse through the main agent client (whose post-success hook
/// fires this runner). `in_flight` is a try_lock guard — the periodic
/// timer and the per-tx hook can both fire concurrently without stacking.
pub struct TopupRunner {
    ledger: Arc<dyn TopupLedger>,
    party_id: String,
    min_cc: Decimal,
    topup_cc: Decimal,
    in_flight: Arc<TokioMutex<()>>,
    fee_demand: Option<Arc<LiquidityManager>>,
    held_since: HeldSince,
    traffic_party: Option<String>,
    dso_party: Option<String>,
    tokens: Vec<TopupToken>,
    stable: Option<StableTopups>,
    skipped: Arc<std::sync::Mutex<HashMap<String, Instant>>>,
    paying: Paying,
    /// Where the payment in flight is kept across restarts.
    payment_file: Option<PathBuf>,
}

impl TopupRunner {
    /// Build a runner if both env vars are set; returns `None` otherwise so
    /// callers can branch cleanly on "auto-topup disabled".
    pub fn new(
        client: DAppProviderClient,
        party_id: String,
        min_cc: Option<Decimal>,
        topup_cc: Option<Decimal>,
    ) -> Option<Self> {
        Self::over(Arc::new(ClientLedger(client)), party_id, min_cc, topup_cc)
    }

    /// [`Self::new`] over any ledger; tops up in CC until [`Self::with_token_topups`].
    pub fn over(
        ledger: Arc<dyn TopupLedger>,
        party_id: String,
        min_cc: Option<Decimal>,
        topup_cc: Option<Decimal>,
    ) -> Option<Self> {
        match (min_cc, topup_cc) {
            (Some(min_cc), Some(topup_cc)) => Some(Self {
                ledger,
                party_id,
                min_cc,
                topup_cc,
                in_flight: Arc::new(TokioMutex::new(())),
                fee_demand: None,
                held_since: HeldSince::default(),
                traffic_party: None,
                dso_party: None,
                tokens: vec![TopupToken::Cc],
                stable: None,
                skipped: Arc::default(),
                paying: Paying::default(),
                payment_file: None,
            }),
            _ => None,
        }
    }

    /// Top up with `tokens` in order; stables need `stable` (the traffic-fee pin, purchases, rates).
    pub fn with_token_topups(mut self, tokens: Vec<TopupToken>, stable: Option<StableTopups>) -> Self {
        self.tokens = tokens;
        self.stable = stable;
        self
    }

    /// Keeps the payment in flight in `file`; one found there is waited for, never paid again.
    pub fn with_payment_file(mut self, file: PathBuf) -> Self {
        match load_payment(&file) {
            Ok(Some(record)) => {
                info!(
                    purchase_id = %log_safe(&record.purchase_id),
                    token = %log_safe(&record.token),
                    committed = record.update_id.is_some(),
                    "Resuming the wait for a top-up payment made before the restart"
                );
                let since = since_of(record.started_at_ms);
                *sync::lock(&self.paying) = Some(OpenPayment { record, since, checked: None, warned: None });
            }
            Ok(None) => {}
            Err(e) => error!(error = %format!("{e:#}"), "The top-up payment file cannot be read; it is ignored and replaced by the next payment"),
        }
        self.payment_file = Some(file);
        self
    }

    /// Edits the payment in flight while it is still `open`.
    fn touch(&self, open: &OpenPayment, edit: impl FnOnce(&mut OpenPayment)) {
        if let Some(slot) = sync::lock(&self.paying).as_mut().filter(|p| p.record.purchase_id == open.record.purchase_id) {
            edit(slot);
        }
    }

    /// Leaves `token` out of the walk for `period`.
    fn skip(&self, token: &str, period: Duration) {
        let now = Instant::now();
        let until = now.checked_add(period).unwrap_or(now);
        sync::lock(&self.skipped).insert(token.to_string(), until);
    }

    fn skipping(&self, token: &str) -> bool {
        let mut skipped = sync::lock(&self.skipped);
        let now = Instant::now();
        skipped.retain(|_, until| *until > now);
        skipped.contains_key(token)
    }

    /// Also top up when balance plus credit is below what in-flight settlements still hold in prepaid.
    pub fn with_fee_demand(mut self, lm: Arc<LiquidityManager>) -> Self {
        self.fee_demand = Some(lm);
        self
    }

    /// The parties the top-up transfer is checked against.
    pub fn with_pins(mut self, traffic_party: Option<String>, dso_party: Option<String>) -> Self {
        self.traffic_party = traffic_party;
        self.dso_party = dso_party;
        self
    }

    fn hold_off(&self) {
        *sync::lock(&self.held_since) = Some(Instant::now());
    }

    fn held_off(&self) -> bool {
        sync::lock(&self.held_since)
            .is_some_and(|at| Instant::now().saturating_duration_since(at) < TOPUP_COOLDOWN)
    }

    /// Check the prepaid traffic balance and top up if it's below the
    /// configured minimum. Best-effort: any error is logged at warn and
    /// swallowed — never fails the caller's tx flow.
    ///
    /// Re-entrancy: if another `maybe_topup` is already running, this call
    /// returns immediately.
    pub async fn maybe_topup(&self) {
        self.check(false).await;
    }

    /// The runner's timer and nudges, which hold off after a submit or a CC shortfall.
    async fn scheduled_topup(&self) {
        self.check(true).await;
    }

    async fn check(&self, honor_hold_off: bool) {
        let Ok(guard) = Arc::clone(&self.in_flight).try_lock_owned() else {
            debug!("Auto-topup already in progress, skipping");
            return;
        };
        if honor_hold_off && self.held_off() {
            debug!("Auto-topup holding off after the last submit or CC shortfall");
            return;
        }
        if let Err(e) = self.run_inner(guard, clock::deadline_after(TOPUP_BUDGET)).await {
            warn!(error = %e, "Auto-topup attempt failed");
        }
    }

    async fn run_inner(&self, guard: OwnedMutexGuard<()>, deadline: Instant) -> Result<()> {
        if self.payment_in_flight(deadline).await {
            debug!("Auto-topup waiting for the last top-up payment to be credited");
            return Ok(());
        }
        let pt = within(deadline, self.ledger.prepaid_balance()).await?;
        let demand = match &self.fee_demand {
            Some(lm) => within(deadline, async { Ok(lm.prepaid_demand().await) }).await?,
            None => Decimal::ZERO,
        };

        if !needs_topup(pt.balance_cc, pt.available_cc, self.min_cc, demand) {
            debug!(
                balance_cc = %pt.balance_cc,
                available_cc = %pt.available_cc,
                min_cc = %self.min_cc,
                demand_cc = %demand,
                "Prepaid balance covers the minimum and pending fees, no topup"
            );
            return Ok(());
        }

        info!(
            balance_cc = %pt.balance_cc,
            available_cc = %pt.available_cc,
            min_cc = %self.min_cc,
            demand_cc = %demand,
            topup_cc = %self.topup_cc,
            "Auto-topping up prepaid traffic balance"
        );
        self.walk("auto-topup", guard, deadline).await
    }

    /// Force a topup unconditionally (skips the balance < min check).
    /// Used by the onboarding flow to seed the prepaid pool with the first
    /// topup right after CC lands. Still respects CC-shortfall warning and
    /// re-entrancy.
    pub async fn force_topup(&self) -> Result<()> {
        let Ok(guard) = Arc::clone(&self.in_flight).try_lock_owned() else {
            return Err(anyhow!("Topup already in progress"));
        };
        let deadline = clock::deadline_after(TOPUP_BUDGET);
        if self.payment_in_flight(deadline).await {
            bail!("a top-up payment is still waiting for its credit");
        }
        self.walk("first-topup", guard, deadline).await
    }

    /// The first token of the list that covers the top-up pays it.
    async fn walk(&self, kind: &str, guard: OwnedMutexGuard<()>, deadline: Instant) -> Result<()> {
        let balances = within(deadline, self.ledger.balances()).await?;
        let mut guard = guard;
        let mut passed: Vec<String> = Vec::new();
        for token in &self.tokens {
            let name = token.name();
            if self.skipping(name) {
                passed.push(format!("{name}: left out after a refusal"));
                continue;
            }
            match token {
                TopupToken::Cc => {
                    let cc = unlocked_cc(&balances);
                    if cc >= self.topup_cc {
                        return self.submit_topup(kind, guard, deadline).await;
                    }
                    passed.push(format!("CC: {cc} unlocked"));
                }
                TopupToken::Stable { .. } => match self.try_stable(kind, token, &balances, guard, deadline).await {
                    Tried::Done => return Ok(()),
                    Tried::Next(back, why) => {
                        guard = back;
                        passed.push(format!("{name}: {why}"));
                    }
                },
            }
        }
        drop(guard);
        self.hold_off();
        warn!(
            topup_cc = %self.topup_cc,
            tokens = %passed.join("; "),
            "Auto-topup skipped: no top-up token covers the configured topup amount"
        );
        Ok(())
    }

    /// One stable: an intent checked against the pin and the registry, then a direct payment.
    async fn try_stable(
        &self,
        kind: &str,
        token: &TopupToken,
        balances: &[TokenBalance],
        guard: OwnedMutexGuard<()>,
        deadline: Instant,
    ) -> Tried {
        let TopupToken::Stable { token: name, wire_id, registry } = token else {
            return Tried::Next(guard, "not a stable".into());
        };
        let Some(stable) = self.stable.as_ref() else {
            return Tried::Next(guard, "the purchase service is unset".into());
        };
        let Some(pin) = (stable.traffic_fee_party)() else {
            return Tried::Next(guard, "PARTY_TRAFFIC_FEE is unset".into());
        };
        let Some(rate) = stable.rates.cc_usd_rate().await else {
            return Tried::Next(guard, "no fresh CC/USD rate".into());
        };
        let Some(amount) = stable_amount(self.topup_cc, rate) else {
            return Tried::Next(guard, format!("no amount at rate {rate}"));
        };
        let held = unlocked_of(balances, wire_id, registry);
        if held < amount {
            return Tried::Next(guard, format!("{held} unlocked, {amount} needed"));
        }
        let want = IntentWant {
            recipient: &pin,
            instrument_id: wire_id,
            instrument_admin: registry,
            token_amount: amount,
            topup_cc: self.topup_cc,
        };
        let order = match buy_intent(stable.purchases.as_ref(), name, &want, deadline).await {
            Ok(order) => order,
            Err(Unbought::Skip(period, why)) => {
                self.skip(name, period);
                return Tried::Next(guard, why);
            }
            Err(Unbought::HoldOff(why)) => {
                self.skip(name, SHORT_SKIP);
                self.hold_off();
                warn!(token = %name, reason = %why, "Auto-topup refused the purchase intent and cancelled it");
                return Tried::Done;
            }
        };
        let record = PaymentRecord {
            purchase_id: order.purchase_id.clone(),
            memo: order.memo.clone(),
            token: name.clone(),
            token_amount: amount,
            update_id: None,
            started_at_ms: clock::now_millis(),
        };
        if let Some(Err(e)) = self.payment_file.as_deref().map(|file| save_payment(file, &record)) {
            crate::control::intent::remove_open_traffic_purchase(&order.memo);
            cancel_intent(stable.purchases.as_ref(), &order.purchase_id, deadline).await;
            self.skip(name, SHORT_SKIP);
            warn!(error = %format!("{e:#}"), token = %name, "Top-up payment not made: it cannot be kept on file");
            return Tried::Next(guard, "the payment cannot be kept on file".into());
        }
        let open = OpenPayment { record, since: Instant::now(), checked: Some(Instant::now()), warned: None };
        *sync::lock(&self.paying) = Some(open.clone());
        info!(
            token = %name,
            token_amount = %amount,
            credit_cc = %order.credit_cc,
            purchase_id = %log_safe(&order.purchase_id),
            kind = %kind,
            "Paying a prepaid traffic purchase"
        );
        let payment = payment_for(&self.party_id, &want, &order);
        let ledger = Arc::clone(&self.ledger);
        let file = self.payment_file.clone();
        let Some(mut task) = supervise::try_spawn("prepaid traffic payment", async move {
            let paid = ledger.pay(payment).await;
            (guard, paid)
        }) else {
            finish_payment(&self.paying, file.as_deref(), &open);
            warn!("no tokio runtime for the top-up payment");
            return Tried::Done;
        };
        let purchase_id = open.record.purchase_id.clone();
        let outcome = tokio::time::timeout_at(deadline, &mut task).await;
        match outcome {
            Ok(Ok((_guard, Ok(update_id)))) => {
                info!(update_id = %update_id, purchase_id = %log_safe(&purchase_id), "Top-up payment submitted; waiting for the credit");
                mark_committed(&self.paying, file.as_deref(), &purchase_id, &update_id);
                Tried::Done
            }
            Ok(Ok((back, Err(e)))) => {
                if crate::ledger_client::is_ambiguous_execute_error(&e) {
                    warn!(error = %format!("{e:#}"), purchase_id = %log_safe(&purchase_id), "Top-up payment outcome unknown; waiting for the credit");
                    return Tried::Done;
                }
                finish_payment(&self.paying, file.as_deref(), &open);
                cancel_intent(stable.purchases.as_ref(), &purchase_id, deadline).await;
                self.skip(name, SHORT_SKIP);
                warn!(error = %format!("{e:#}"), token = %name, "Top-up payment not made; trying the next token");
                Tried::Next(back, "the payment was not made".into())
            }
            Ok(Err(e)) => {
                warn!(purchase_id = %log_safe(&purchase_id), "Top-up payment task ended: {e}");
                Tried::Done
            }
            Err(_) => {
                warn!(
                    purchase_id = %log_safe(&purchase_id),
                    "Top-up payment still running after {}s; it continues in the background",
                    TOPUP_BUDGET.as_secs()
                );
                let (paying, api) = (Arc::clone(&self.paying), Arc::clone(&stable.purchases));
                let _ = supervise::try_spawn("top-up payment outcome", async move {
                    match task.await {
                        Ok((_guard, Err(e))) if !crate::ledger_client::is_ambiguous_execute_error(&e) => {
                            warn!(error = %format!("{e:#}"), "Top-up payment failed");
                            finish_payment(&paying, file.as_deref(), &open);
                            let cancel_by = clock::deadline_after(crate::traffic_purchase::PURCHASE_CALL_TIMEOUT);
                            cancel_intent(api.as_ref(), &purchase_id, cancel_by).await;
                        }
                        Ok((_guard, Ok(update_id))) => {
                            info!(update_id = %update_id, "Top-up payment submitted late");
                            mark_committed(&paying, file.as_deref(), &purchase_id, &update_id);
                        }
                        Ok((_guard, Err(e))) => warn!(error = %format!("{e:#}"), "Top-up payment outcome unknown"),
                        Err(e) => warn!("Top-up payment task ended: {e}"),
                    }
                });
                Tried::Done
            }
        }
    }

    /// True while a stable payment waits for its purchase to read credited or rejected; one of
    /// unknown outcome also ends when its purchase reads open after [`UNKNOWN_OUTCOME_WAIT`].
    async fn payment_in_flight(&self, deadline: Instant) -> bool {
        let Some(open) = sync::lock(&self.paying).clone() else {
            return false;
        };
        let now = Instant::now();
        if open.checked.is_some_and(|at| now.saturating_duration_since(at) < STATUS_POLL_EVERY) {
            return true;
        }
        self.touch(&open, |p| p.checked = Some(now));
        let purchase_id = log_safe(&open.record.purchase_id);
        let file = self.payment_file.as_deref();
        let purchase = match self.stable.as_ref() {
            Some(stable) => match tokio::time::timeout_at(deadline, stable.purchases.get(&open.record.purchase_id)).await {
                Ok(Ok(purchase)) => Some(purchase),
                Ok(Err(e)) => {
                    debug!(purchase_id = %purchase_id, error = %e, "Top-up purchase status unavailable");
                    None
                }
                Err(_) => None,
            },
            None => None,
        };
        if let Some(purchase) = purchase.as_ref() {
            match crate::traffic_purchase::purchase_state(&purchase.status) {
                PurchaseState::Credited => {
                    let credited = purchase.credited_cc.as_deref().unwrap_or(&purchase.credit_cc);
                    info!(purchase_id = %purchase_id, credited_cc = %log_safe(credited), "Top-up purchase credited");
                    finish_payment(&self.paying, file, &open);
                    return false;
                }
                PurchaseState::Rejected => {
                    let reason = purchase.reject_reason.as_deref().unwrap_or("");
                    warn!(purchase_id = %purchase_id, reason = %log_safe(reason), token = %open.record.token, "Top-up purchase rejected");
                    finish_payment(&self.paying, file, &open);
                    self.skip(&open.record.token, SHORT_SKIP);
                    return false;
                }
                PurchaseState::Open => {}
            }
        }
        let waited = now.saturating_duration_since(open.since);
        if let Some(purchase) = purchase.as_ref().filter(|_| open.record.update_id.is_none() && waited >= UNKNOWN_OUTCOME_WAIT) {
            warn!(
                purchase_id = %purchase_id,
                status = %log_safe(&purchase.status),
                "Top-up payment of unknown outcome not credited within {}s; topping up again",
                UNKNOWN_OUTCOME_WAIT.as_secs()
            );
            finish_payment(&self.paying, file, &open);
            return false;
        }
        if waited >= STILL_WAITING_WARN && open.warned.is_none_or(|at| now.saturating_duration_since(at) >= STILL_WAITING_WARN) {
            self.touch(&open, |p| p.warned = Some(now));
            match open.record.update_id.as_deref() {
                Some(update_id) => warn!(
                    purchase_id = %purchase_id,
                    update_id = %log_safe(update_id),
                    waited_secs = waited.as_secs(),
                    "Top-up payment committed but not credited yet; no other top-up runs until it is (credits never expire)"
                ),
                None => warn!(
                    purchase_id = %purchase_id,
                    waited_secs = waited.as_secs(),
                    "Top-up payment of unknown outcome still waiting for its credit or its purchase status; no other top-up runs until then"
                ),
            }
        }
        true
    }
    /// The submit runs in its own task, holding `guard` until it ends; it is
    /// waited for until `deadline` and never cancelled.
    async fn submit_topup(
        &self,
        command_prefix: &str,
        guard: OwnedMutexGuard<()>,
        deadline: Instant,
    ) -> Result<()> {
        let amount_str = self.topup_cc.to_string();
        let command_id = format!("{}-{}", command_prefix, clock::now_millis());
        let expectation = OperationExpectation::PrepayTraffic {
            sender_party: self.party_id.clone(),
            amount: amount_str.clone(),
            command_id: command_id.clone(),
            receiver_party: self.traffic_party.clone(),
            dso_party: self.dso_party.clone(),
        };
        let request = PrepareTransactionRequest {
            operation: TransactionOperation::PrepayTraffic as i32,
            params: Some(Params::PrepayTraffic(PrepayTrafficParams {
                amount: amount_str,
                description: Some(command_prefix.to_string()),
                command_id,
                amulet_cids: vec![],
            })),
            request_signature: None,
            controller_command: None,
        };
        let ledger = Arc::clone(&self.ledger);
        let submit = async move { ledger.prepay(request, expectation).await };
        let task = spawn_submit(guard, Arc::clone(&self.held_since), submit)?;
        wait_submit(command_prefix.to_string(), self.topup_cc, task, deadline).await
    }

    /// Read-only view: return the current prepaid traffic balance via the
    /// runner's own client. Used by onboarding to print the post-topup
    /// state without standing up another client.
    pub async fn get_balance(&self) -> Result<crate::PrepaidTrafficBalance> {
        within(clock::deadline_after(TOPUP_BUDGET), self.ledger.prepaid_balance()).await
    }

    /// Spawn the runner's background task. Returns a `TopupTrigger` handle
    /// that the per-tx hook uses to nudge the runner without spawning a
    /// new task itself (avoids the Send bound on submit_transaction's
    /// future).
    ///
    /// The background task:
    /// - Listens on an mpsc channel for nudges from the per-tx hook.
    /// - Checks every 10 minutes regardless.
    ///
    /// Both drop the nudges already queued first, so a flood of nudges costs one check.
    pub fn spawn(self: Arc<Self>, shutdown: Shutdown) -> Result<TopupTrigger> {
        let (tx, rx) = mpsc::channel::<()>(NUDGE_SLOTS);
        let rx = Arc::new(TokioMutex::new(rx));
        let s = shutdown.clone();
        supervise::spawn_supervised("topup runner", shutdown, Policy::Restart, move || {
            run(Arc::clone(&self), Arc::clone(&rx), s.clone())
        })?;
        Ok(TopupTrigger { tx })
    }
}

async fn run(runner: Arc<TopupRunner>, rx: Arc<TokioMutex<mpsc::Receiver<()>>>, shutdown: Shutdown) {
    let mut rx = rx.lock().await;
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume immediate-fire initial tick
    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait() => {
                info!("Topup runner shutting down");
                break;
            }
            _ = ticker.tick() => {
                drain(&mut rx);
                runner.scheduled_topup().await;
            }
            Some(_) = rx.recv() => {
                drain(&mut rx);
                runner.scheduled_topup().await;
            }
            else => break,
        }
    }
}

/// Drop the nudges already queued; the check that follows covers them.
fn drain(rx: &mut mpsc::Receiver<()>) {
    for _ in 0..NUDGE_SLOTS {
        if rx.try_recv().is_err() {
            break;
        }
    }
}

/// A stable purchase the CLI makes.
#[derive(Clone, Debug)]
pub struct StablePurchase<'a> {
    pub token: &'a str,
    pub wire_id: &'a str,
    pub registry: &'a str,
    pub traffic_fee_party: &'a str,
    pub credit_cc: Decimal,
    pub cc_usd_rate: Decimal,
}

/// A paid purchase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoughtTraffic {
    pub purchase_id: String,
    pub memo: String,
    pub update_id: String,
    pub token_amount: Decimal,
    pub credit_cc: Decimal,
}

/// Buys `credit_cc` of prepaid traffic once with a stable: the balance, a checked intent,
/// then the direct payment. A payment refused before signing cancels the intent.
pub async fn buy_traffic(
    ledger: &dyn TopupLedger,
    api: &dyn PurchaseApi,
    party: &str,
    buy: &StablePurchase<'_>,
    deadline: Instant,
) -> Result<BoughtTraffic> {
    let amount = stable_amount(buy.credit_cc, buy.cc_usd_rate)
        .ok_or_else(|| anyhow!("no {} amount for {} CC at {} USD per CC", buy.token, buy.credit_cc, buy.cc_usd_rate))?;
    let balances = within(deadline, ledger.balances()).await?;
    let held = unlocked_of(&balances, buy.wire_id, buy.registry);
    if held < amount {
        bail!("{held} {} unlocked, {amount} needed for {} CC", buy.token, buy.credit_cc);
    }
    let want = IntentWant {
        recipient: buy.traffic_fee_party,
        instrument_id: buy.wire_id,
        instrument_admin: buy.registry,
        token_amount: amount,
        topup_cc: buy.credit_cc,
    };
    let order = match buy_intent(api, buy.token, &want, deadline).await {
        Ok(order) => order,
        Err(Unbought::Skip(_, why)) => bail!("no purchase intent: {why}"),
        Err(Unbought::HoldOff(why)) => bail!("the purchase intent was refused and cancelled: {why}"),
    };
    let paid = tokio::time::timeout_at(deadline, ledger.pay(payment_for(party, &want, &order))).await;
    let id = log_safe(&order.purchase_id);
    match paid {
        Ok(Ok(update_id)) => Ok(BoughtTraffic {
            purchase_id: order.purchase_id,
            memo: order.memo,
            update_id,
            token_amount: amount,
            credit_cc: order.credit_cc,
        }),
        Ok(Err(e)) if !crate::ledger_client::is_ambiguous_execute_error(&e) => {
            crate::control::intent::remove_open_traffic_purchase(&order.memo);
            cancel_intent(api, &order.purchase_id, clock::deadline_after(crate::traffic_purchase::PURCHASE_CALL_TIMEOUT)).await;
            Err(e.context(format!("the payment for purchase {id} was not made; the intent is cancelled")))
        }
        Ok(Err(e)) => Err(e.context(format!("the payment for purchase {id} may still land and be credited"))),
        Err(_) => bail!("the payment for purchase {id} did not finish in time; it may still land and be credited"),
    }
}

/// Polls a purchase until it is credited or rejected, at most `wait`; the last state read.
pub async fn wait_for_credit(api: &dyn PurchaseApi, purchase_id: &str, wait: Duration) -> Option<AgentTrafficPurchase> {
    const POLL_EVERY: Duration = Duration::from_secs(3);
    let deadline = clock::deadline_after(wait);
    let mut last = None;
    loop {
        if let Ok(Ok(purchase)) = tokio::time::timeout_at(deadline, api.get(purchase_id)).await {
            if crate::traffic_purchase::purchase_state(&purchase.status) != PurchaseState::Open {
                return Some(purchase);
            }
            last = Some(purchase);
        }
        let now = Instant::now();
        if now >= deadline {
            return last;
        }
        tokio::time::sleep_until(now.checked_add(POLL_EVERY).unwrap_or(deadline).min(deadline)).await;
    }
}

/// Below the floor, or below what in-flight settlements still hold in prepaid.
fn needs_topup(balance: Decimal, available: Decimal, min: Decimal, demand: Decimal) -> bool {
    balance < min || available < demand
}

/// `call` within the attempt's deadline.
async fn within<T>(deadline: Instant, call: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout_at(deadline, call)
        .await
        .map_err(|_| anyhow!("no answer within the {}s topup budget", TOPUP_BUDGET.as_secs()))?
}

/// Starts the hold-off when dropped: after a submit returns, panics or is cancelled.
struct HoldOffOnDrop(HeldSince);

impl Drop for HoldOffOnDrop {
    fn drop(&mut self) {
        *sync::lock(&self.0) = Some(Instant::now());
    }
}

/// Run `submit` in its own task, holding `guard` until it ends; the hold-off
/// starts only then, so a credit that lands late still has the full cooldown.
fn spawn_submit<F>(
    guard: OwnedMutexGuard<()>,
    held_since: HeldSince,
    submit: F,
) -> Result<JoinHandle<Result<String>>>
where
    F: Future<Output = Result<String>> + Send + 'static,
{
    supervise::try_spawn("prepaid traffic topup", async move {
        let _guard = guard;
        // Dropped before the guard, so the hold-off is in place when the lock frees
        let _hold = HoldOffOnDrop(held_since);
        submit.await
    })
    .ok_or_else(|| anyhow!("no tokio runtime for the topup submit"))
}

/// Wait for the submit until `deadline`; after that it is left to finish and
/// its outcome is logged when it does.
async fn wait_submit(
    kind: String,
    topup_cc: Decimal,
    mut task: JoinHandle<Result<String>>,
    deadline: Instant,
) -> Result<()> {
    match tokio::time::timeout_at(deadline, &mut task).await {
        Ok(joined) => submitted(&kind, topup_cc, joined),
        Err(_) => {
            warn!(
                kind = %kind,
                "Topup submit still running after {}s; it continues in the background",
                TOPUP_BUDGET.as_secs()
            );
            let _ = supervise::try_spawn("topup outcome", async move {
                if let Err(e) = submitted(&kind, topup_cc, task.await) {
                    warn!(error = %e, kind = %kind, "Topup submit failed");
                }
            });
            Ok(())
        }
    }
}

fn submitted(kind: &str, topup_cc: Decimal, joined: Result<Result<String>, JoinError>) -> Result<()> {
    let update_id = joined.map_err(|e| anyhow!("topup submit task failed: {e}"))??;
    info!(
        update_id = %update_id,
        topup_cc = %topup_cc,
        kind = %kind,
        "Topup submitted"
    );
    Ok(())
}

/// Lightweight handle attached to the main `DAppProviderClient` so the
/// post-success hook can fire-and-forget a topup nudge without spawning a
/// new task itself. Cloning is cheap (just an `mpsc::Sender`).
#[derive(Clone)]
pub struct TopupTrigger {
    tx: mpsc::Sender<()>,
}

impl TopupTrigger {
    /// Try to nudge the topup runner. Non-blocking; a full channel drops the
    /// nudge, which is harmless because queued nudges are merged into one check.
    pub fn nudge(&self) {
        let _ = self.tx.try_send(());
    }

    #[cfg(test)]
    pub(crate) fn from_sender(tx: mpsc::Sender<()>) -> Self {
        Self { tx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_logic::config::BaseConfig;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn runner() -> TopupRunner {
        runner_at("http://127.0.0.1:1")
    }

    fn runner_at(url: &str) -> TopupRunner {
        let config = BaseConfig::test_minimal().unwrap();
        let channel = tonic::transport::Endpoint::from_shared(url.to_string()).unwrap().connect_lazy();
        let client = DAppProviderClient::from_channel(
            channel,
            &config.party_id,
            &config.role,
            &config.private_key,
            config.token_ttl_secs,
            None,
            &config.ledger_service_public_key,
        )
        .unwrap();
        TopupRunner::new(client, config.party_id, Some(Decimal::ONE), Some(Decimal::TEN)).unwrap()
    }

    fn balances(balance: &'static str, available: &'static str, cc: &'static str) -> crate::test_util::Fake {
        crate::test_util::Fake::Balances { balance, available, cc }
    }

    /// Move the hold-off back by the cooldown, so it has just run out.
    fn backdate(runner: &TopupRunner) {
        *sync::lock(&runner.held_since) = Instant::now().checked_sub(TOPUP_COOLDOWN);
    }

    // A slow submit is waited for up to the budget, then left running and
    // holding off the next attempt until it ends; the hold-off starts then
    #[tokio::test(start_paused = true)]
    async fn a_slow_submit_is_left_running_and_holds_off_the_next_attempt() {
        let lock = Arc::new(TokioMutex::new(()));
        let held = HeldSince::default();
        let done = Arc::new(AtomicBool::new(false));
        let d = done.clone();
        let task = spawn_submit(lock.clone().try_lock_owned().unwrap(), held.clone(), async move {
            tokio::time::sleep(Duration::from_secs(600)).await;
            d.store(true, Ordering::SeqCst);
            Ok("u1".to_string())
        })
        .unwrap();
        let deadline = Instant::now() + TOPUP_BUDGET;
        wait_submit("auto-topup".into(), Decimal::TEN, task, deadline).await.unwrap();
        assert!(Instant::now() >= deadline);
        assert!(lock.try_lock().is_err(), "the running submit holds the in-flight lock");
        assert!(sync::lock(&held).is_none(), "the hold-off waits for the submit to end");
        tokio::time::sleep(Duration::from_secs(400)).await;
        assert!(done.load(Ordering::SeqCst), "the submit was not cancelled");
        assert!(lock.try_lock().is_ok());
        assert!(sync::lock(&held).is_some(), "the hold-off starts when the submit ends");
    }

    // The runner's own submit: past its deadline it keeps running and keeps the lock
    #[tokio::test]
    async fn a_topup_submit_past_its_deadline_keeps_the_lock_until_it_ends() {
        let slow = crate::test_util::Fake::Slow(Duration::from_secs(1));
        let ledger = crate::test_util::FakeLedger::start(slow).await;
        let runner = runner_at(&ledger.url);
        let guard = Arc::clone(&runner.in_flight).try_lock_owned().unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);
        runner.submit_topup("auto-topup", guard, deadline).await.unwrap();
        assert!(runner.in_flight.try_lock().is_err(), "the running submit holds the in-flight lock");
        let freed = tokio::time::timeout(Duration::from_secs(30), async {
            while runner.in_flight.try_lock().is_err() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        freed.await.expect("the lock is free once the submit ends");
        assert_eq!(ledger.calls("PrepareTransaction"), 1, "the submit reached the ledger");
        ledger.stop().await;
    }

    #[tokio::test]
    async fn submit_failures_within_the_budget_are_returned() {
        let lock = Arc::new(TokioMutex::new(()));
        let deadline = clock::deadline_after(TOPUP_BUDGET);
        let held = HeldSince::default();
        let failed = spawn_submit(lock.clone().try_lock_owned().unwrap(), held.clone(), async { Err(anyhow!("rejected")) }).unwrap();
        let err = wait_submit("auto-topup".into(), Decimal::TEN, failed, deadline).await.unwrap_err();
        assert_eq!(err.to_string(), "rejected");
        assert!(sync::lock(&held).is_some(), "a refused submit also holds off the next check");
        let held = HeldSince::default();
        let panicked = spawn_submit(lock.clone().try_lock_owned().unwrap(), held.clone(), async { panic!("submit bug") }).unwrap();
        let err = wait_submit("auto-topup".into(), Decimal::TEN, panicked, deadline).await.unwrap_err();
        assert!(err.to_string().starts_with("topup submit task failed"), "{err}");
        assert!(lock.try_lock().is_ok(), "the lock is released after a panic");
        assert!(sync::lock(&held).is_some(), "a panicked submit also holds off the next check");
    }

    #[tokio::test]
    async fn an_attempt_in_progress_makes_the_next_one_return_at_once() {
        let runner = runner();
        let _held = runner.in_flight.clone().try_lock_owned().unwrap();
        tokio::time::timeout(Duration::from_secs(1), runner.maybe_topup()).await.unwrap();
        let err = runner.force_topup().await.unwrap_err();
        assert_eq!(err.to_string(), "Topup already in progress");
    }

    #[tokio::test(start_paused = true)]
    async fn a_balance_read_that_never_answers_is_bounded() {
        let err = within(Instant::now() + TOPUP_BUDGET, std::future::pending::<Result<()>>()).await.unwrap_err();
        assert!(err.to_string().contains("300s topup budget"), "{err}");
    }

    #[test]
    fn a_topup_is_needed_below_the_floor_or_short_of_the_pending_fee() {
        let d = |s: &str| Decimal::from_str_exact(s).unwrap();
        // balance, available, min, demand
        assert!(needs_topup(d("9"), d("9"), d("10"), d("0")), "below the floor");
        assert!(!needs_topup(d("10"), d("10"), d("10"), d("0")), "at the floor, nothing pending");
        assert!(needs_topup(d("40"), d("40"), d("10"), d("41")), "short of the largest pending fee");
        assert!(!needs_topup(d("40"), d("50"), d("10"), d("41")), "credit covers the pending fee");
    }

    #[tokio::test(start_paused = true)]
    async fn a_topup_holds_off_the_next_one_for_the_cooldown() {
        let runner = runner();
        assert!(!runner.held_off());
        runner.hold_off();
        assert!(runner.held_off());
        tokio::time::sleep(TOPUP_COOLDOWN).await;
        assert!(!runner.held_off());
    }

    // Settlement steps nudge the runner; only its own checks honour the hold-off
    #[tokio::test]
    async fn scheduled_checks_hold_off_but_a_direct_call_does_not() {
        let ledger = crate::test_util::FakeLedger::start(balances("100", "100", "100")).await;
        let runner = runner_at(&ledger.url);
        runner.hold_off();
        runner.scheduled_topup().await;
        assert_eq!(ledger.calls("GetPrepaidTrafficBalance"), 0);
        runner.maybe_topup().await;
        assert_eq!(ledger.calls("GetPrepaidTrafficBalance"), 1, "a direct call checks at once");
        backdate(&runner);
        runner.scheduled_topup().await;
        assert_eq!(ledger.calls("GetPrepaidTrafficBalance"), 2);
        assert_eq!(ledger.calls("PrepareTransaction"), 0, "the balance is above the floor");
        ledger.stop().await;
    }

    // A fee reserved for an in-flight settlement above the available balance tops up
    #[tokio::test]
    async fn a_reserved_fee_above_the_available_balance_tops_up() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_prepaid(Decimal::from(100), std::time::Instant::now()).await;
        lm.try_commit("p1", "USDCx", Decimal::from(1000), Decimal::from(20)).await.unwrap();
        for (with_demand, topups) in [(true, 1), (false, 0)] {
            let ledger = crate::test_util::FakeLedger::start(balances("10", "10", "100")).await;
            let mut runner = runner_at(&ledger.url);
            if with_demand {
                runner = runner.with_fee_demand(lm.clone());
            }
            runner.maybe_topup().await;
            assert_eq!(ledger.calls("PrepareTransaction"), topups, "with_demand={with_demand}");
            assert_eq!(ledger.calls("GetBalances"), topups, "with_demand={with_demand}");
            ledger.stop().await;
        }
    }

    // Ten pending fees of 14.7 CC each need a top-up though each alone fits in 50
    #[tokio::test]
    async fn the_demand_is_every_pending_fee_not_the_largest() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(1000)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_prepaid(Decimal::from(50), std::time::Instant::now()).await;
        lm.set_auto_topup(Some(Decimal::from(125))).await;
        let fee = Decimal::from_str_exact("14.7").unwrap();
        for i in 0..10 {
            lm.try_commit(&format!("p{i}"), "USDCx", Decimal::ONE, fee).await.unwrap();
        }
        let demand = lm.prepaid_demand().await;
        assert_eq!(demand, Decimal::from(147));
        assert!(needs_topup(Decimal::from(50), Decimal::from(50), Decimal::from(20), demand));
    }

    // A wallet short of CC used to be read again, with a WARN, on every settlement step
    #[tokio::test]
    async fn a_cc_short_wallet_is_checked_once_per_hold_off() {
        let ledger = crate::test_util::FakeLedger::start(balances("0", "0", "5")).await;
        let runner = runner_at(&ledger.url);
        runner.scheduled_topup().await;
        runner.scheduled_topup().await;
        assert_eq!(ledger.calls("GetBalances"), 1);
        assert_eq!(ledger.calls("PrepareTransaction"), 0, "5 CC is short of the 10 CC topup");
        backdate(&runner);
        runner.scheduled_topup().await;
        assert_eq!(ledger.calls("GetBalances"), 2);
        ledger.stop().await;
    }

    // A burst of settlement steps costs one balance check
    #[tokio::test]
    async fn a_burst_of_nudges_costs_one_check() {
        let ledger = crate::test_util::FakeLedger::start(balances("100", "100", "100")).await;
        let shutdown = Shutdown::new();
        let trigger = Arc::new(runner_at(&ledger.url)).spawn(shutdown.clone()).unwrap();
        for _ in 0..5 {
            trigger.nudge();
        }
        let checked = tokio::time::timeout(Duration::from_secs(10), async {
            while ledger.calls("GetPrepaidTrafficBalance") == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        checked.await.expect("the nudges were checked");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(ledger.calls("GetPrepaidTrafficBalance"), 1);
        shutdown.signal();
        ledger.stop().await;
    }

    // ---- token top-ups -------------------------------------------------

    use crate::traffic_purchase::{PurchaseError, LONG_SKIP};
    use anyhow::Context;
    use std::sync::atomic::AtomicUsize;
    use tx_verifier::facts::ValueFlow;

    const PIN: &str = "traffic-fee::1220aa";
    const TUSDT_WIRE: (&str, &str) = ("tusdt-wire", "tusdt-registry::1220bb");
    const USDCX_WIRE: (&str, &str) = ("USDCx", "usdcx-registry::1220cc");

    fn d(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    fn holding(wire: (&str, &str), unlocked: &str) -> TokenBalance {
        TokenBalance {
            instrument_id: wire.0.into(),
            instrument_admin: wire.1.into(),
            total_amount: unlocked.into(),
            locked_amount: "0".into(),
            unlocked_amount: unlocked.into(),
            is_canton_coin: false,
        }
    }

    fn cc_holding(unlocked: &str) -> TokenBalance {
        TokenBalance { is_canton_coin: true, ..holding(("Amulet", "dso::1220dd"), unlocked) }
    }

    /// A ledger whose payments show a pending transfer for `pending_for`, else a direct holding.
    #[derive(Default)]
    struct FakeBook {
        balances: Vec<TokenBalance>,
        pending_for: Option<&'static str>,
        /// Signed payments end with an execute of unknown outcome.
        ambiguous: bool,
        payments: std::sync::Mutex<Vec<Payment>>,
        signed: std::sync::Mutex<Vec<String>>,
        prepays: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TopupLedger for FakeBook {
        async fn prepaid_balance(&self) -> Result<PrepaidTrafficBalance> {
            Ok(PrepaidTrafficBalance {
                balance_cc: Decimal::TEN,
                credit_limit_cc: Decimal::ZERO,
                available_cc: Decimal::TEN,
                total_credited_cc: Decimal::ZERO,
                total_debited_cc: Decimal::ZERO,
                orderbook_fee_discount_pct: None,
            })
        }
        async fn balances(&self) -> Result<Vec<TokenBalance>> {
            Ok(self.balances.clone())
        }
        async fn prepay(&self, _: PrepareTransactionRequest, _: OperationExpectation) -> Result<String> {
            self.prepays.fetch_add(1, Ordering::SeqCst);
            Ok("u-cc".into())
        }
        async fn pay(&self, payment: Payment) -> Result<String> {
            let Some(Params::TransferCip56(p)) = payment.request.params.clone() else { bail!("not a transfer") };
            let kind = if self.pending_for == Some(p.instrument_id.as_str()) { FlowKind::PendingTransfer } else { FlowKind::Holding };
            let flow = ValueFlow {
                to: p.receiver_party.clone(),
                instrument_admin: p.instrument_admin.clone(),
                instrument_id: p.instrument_id.clone(),
                amount: d(&p.amount),
                kind,
            };
            let facts = TxFacts { outflows: vec![flow], memos: p.reference.iter().cloned().collect(), ..TxFacts::default() };
            sync::lock(&self.payments).push(payment.clone());
            check_payment(&Ok(facts), &payment.check).context("refused before signing")?;
            sync::lock(&self.signed).push(payment.check.memo.clone());
            if self.ambiguous {
                bail!("{}: the execute timed out", crate::ledger_client::EXECUTE_OUTCOME_UNKNOWN);
            }
            Ok(format!("u-{}", p.instrument_id))
        }
    }

    /// A purchase service quoting `credit_pct` of the CC asked, naming `recipient` and `catalog`.
    struct FakePurchases {
        prefix: String,
        topup_cc: Decimal,
        recipient: String,
        catalog: HashMap<&'static str, (&'static str, &'static str)>,
        credit_pct: Decimal,
        refuse: HashMap<&'static str, PurchaseError>,
        memo_prefix: &'static str,
        status: std::sync::Mutex<&'static str>,
        asked: std::sync::Mutex<Vec<String>>,
        created: std::sync::Mutex<Vec<String>>,
        cancelled: std::sync::Mutex<Vec<String>>,
        gets: AtomicUsize,
    }

    /// Purchase ids unique within this process, whose open memos are shared.
    static NEXT_FAKE: AtomicUsize = AtomicUsize::new(0);

    impl FakePurchases {
        fn new(topup_cc: Decimal) -> Self {
            Self {
                prefix: format!("fake{}", NEXT_FAKE.fetch_add(1, Ordering::SeqCst)),
                topup_cc,
                recipient: PIN.into(),
                catalog: HashMap::from([("TUSDT", TUSDT_WIRE), ("USDCx", USDCX_WIRE)]),
                credit_pct: Decimal::ONE_HUNDRED,
                refuse: HashMap::new(),
                memo_prefix: "agent-traffic:",
                status: std::sync::Mutex::new("pending"),
                asked: Default::default(),
                created: Default::default(),
                cancelled: Default::default(),
                gets: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl PurchaseApi for FakePurchases {
        async fn create(&self, token: &str, token_amount: &str) -> Result<CreateTrafficPurchaseResponse, PurchaseError> {
            sync::lock(&self.asked).push(token.to_string());
            if let Some(refusal) = self.refuse.get(token) {
                return Err(refusal.clone());
            }
            let n = {
                let mut created = sync::lock(&self.created);
                created.push(token.to_string());
                created.len()
            };
            let id = format!("{}-{token}-{n}", self.prefix);
            let (wire, admin) = self.catalog.get(token).copied().unwrap_or_default();
            let credit = self.topup_cc * self.credit_pct / Decimal::ONE_HUNDRED;
            Ok(CreateTrafficPurchaseResponse {
                purchase: Some(AgentTrafficPurchase {
                    purchase_id: id.clone(),
                    status: "pending".into(),
                    token: token.into(),
                    token_amount: token_amount.into(),
                    credit_cc: credit.to_string(),
                    ..Default::default()
                }),
                recipient_party: self.recipient.clone(),
                instrument_id: wire.into(),
                instrument_admin: admin.into(),
                transfer_kind: "utility".into(),
                memo: format!("{}{id}", self.memo_prefix),
            })
        }
        async fn get(&self, purchase_id: &str) -> Result<AgentTrafficPurchase, PurchaseError> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            let status = sync::lock(&self.status).to_string();
            if status == "unavailable" {
                return Err(PurchaseError::Failed("unavailable".into()));
            }
            Ok(AgentTrafficPurchase { purchase_id: purchase_id.into(), status, ..Default::default() })
        }
        async fn cancel(&self, purchase_id: &str) -> Result<AgentTrafficPurchase, PurchaseError> {
            sync::lock(&self.cancelled).push(purchase_id.to_string());
            Ok(AgentTrafficPurchase { purchase_id: purchase_id.into(), status: "cancelled".into(), ..Default::default() })
        }
    }

    fn stable(token: &str, wire: (&str, &str)) -> TopupToken {
        TopupToken::Stable { token: token.into(), wire_id: wire.0.into(), registry: wire.1.into() }
    }

    /// 175 CC at 0.075 USD per CC is 13.125 of a stable; the prepaid balance (10) is under the 50 floor.
    async fn token_runner(book: &Arc<FakeBook>, api: &Arc<FakePurchases>) -> TopupRunner {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_usd_rate(d("0.075")).await;
        let ledger: Arc<dyn TopupLedger> = book.clone();
        let purchases: Arc<dyn PurchaseApi> = api.clone();
        TopupRunner::over(ledger, "agent::1220ee".into(), Some(Decimal::from(50)), Some(Decimal::from(175)))
            .unwrap()
            .with_token_topups(
                vec![stable("TUSDT", TUSDT_WIRE), stable("USDCx", USDCX_WIRE), TopupToken::Cc],
                Some(StableTopups { traffic_fee_party: fixed_pin(PIN), purchases, rates: lm }),
            )
    }

    fn book(tusdt: &str, usdcx: &str) -> FakeBook {
        book_with_cc(tusdt, usdcx, "1000")
    }

    fn book_with_cc(tusdt: &str, usdcx: &str, cc: &str) -> FakeBook {
        FakeBook {
            balances: vec![holding(TUSDT_WIRE, tusdt), holding(USDCX_WIRE, usdcx), cc_holding(cc)],
            ..FakeBook::default()
        }
    }

    fn forget_memos(book: &FakeBook) {
        for memo in sync::lock(&book.signed).iter() {
            crate::control::intent::remove_open_traffic_purchase(memo);
        }
    }

    #[tokio::test]
    async fn the_first_covering_token_is_used() {
        let book = Arc::new(book("10", "20"));
        let api = Arc::new(FakePurchases::new(Decimal::from(175)));
        token_runner(&book, &api).await.maybe_topup().await;
        assert_eq!(*sync::lock(&api.created), vec!["USDCx"], "TUSDT (10) is short of 13.125");
        let signed = sync::lock(&book.signed).clone();
        assert_eq!(signed.len(), 1);
        let payment = sync::lock(&book.payments)[0].clone();
        let Some(Params::TransferCip56(p)) = payment.request.params else { panic!("a CIP-56 transfer") };
        assert!(p.direct_only, "never a two-step offer");
        assert_eq!((p.receiver_party.as_str(), p.amount.as_str()), (PIN, "13.125"));
        assert_eq!((p.instrument_id.as_str(), p.instrument_admin.as_str()), USDCX_WIRE);
        assert_eq!(p.reference.as_deref(), Some(signed[0].as_str()), "the intent's memo is the reference");
        assert!(crate::control::intent::is_open_traffic_purchase(&signed[0]), "open while in flight");
        assert_eq!(book.prepays.load(Ordering::SeqCst), 0, "CC is not used when a stable covers");
        forget_memos(&book);
    }

    #[tokio::test]
    async fn a_pending_transfer_shape_signs_nothing_and_the_next_token_is_tried() {
        let book = Arc::new(FakeBook { pending_for: Some(TUSDT_WIRE.0), ..book("20", "20") });
        let api = Arc::new(FakePurchases::new(Decimal::from(175)));
        token_runner(&book, &api).await.maybe_topup().await;
        assert_eq!(*sync::lock(&api.created), vec!["TUSDT", "USDCx"]);
        assert_eq!(sync::lock(&book.payments).len(), 2, "both payments were prepared");
        let signed = sync::lock(&book.signed).clone();
        assert_eq!(signed.len(), 1, "only the direct one was signed");
        assert!(signed[0].contains("-USDCx-"), "{signed:?}");
        let cancelled = sync::lock(&api.cancelled).clone();
        assert_eq!(cancelled.len(), 1);
        assert!(cancelled[0].contains("-TUSDT-"), "the unpaid TUSDT intent is cancelled: {cancelled:?}");
        let tusdt_memo = format!("agent-traffic:{}", cancelled[0]);
        assert!(!crate::control::intent::is_open_traffic_purchase(&tusdt_memo));
        forget_memos(&book);
    }

    #[tokio::test(start_paused = true)]
    async fn unimplemented_falls_back_to_cc_and_leaves_the_stables_out_for_30_minutes() {
        let book = Arc::new(book("20", "20"));
        let mut fake = FakePurchases::new(Decimal::from(175));
        fake.refuse = HashMap::from([("TUSDT", PurchaseError::Unimplemented), ("USDCx", PurchaseError::Refused(4141))]);
        let api = Arc::new(fake);
        let runner = token_runner(&book, &api).await;
        runner.maybe_topup().await;
        assert_eq!(*sync::lock(&api.asked), vec!["TUSDT", "USDCx"]);
        assert_eq!(book.prepays.load(Ordering::SeqCst), 1, "CC tops up instead");
        tokio::time::advance(Duration::from_secs(10 * 60)).await;
        runner.maybe_topup().await;
        assert_eq!(sync::lock(&api.asked).len(), 2, "still left out after 10 min");
        assert_eq!(book.prepays.load(Ordering::SeqCst), 2);
        tokio::time::advance(LONG_SKIP).await;
        runner.maybe_topup().await;
        assert_eq!(sync::lock(&api.asked).len(), 4, "asked again after 30 min");
    }

    #[tokio::test]
    async fn an_unfair_quote_is_cancelled_and_holds_off() {
        let book = Arc::new(book("20", "20"));
        let mut fake = FakePurchases::new(Decimal::from(175));
        fake.credit_pct = Decimal::from(96);
        let api = Arc::new(fake);
        let runner = token_runner(&book, &api).await;
        runner.maybe_topup().await;
        assert_eq!(*sync::lock(&api.created), vec!["TUSDT"]);
        assert_eq!(sync::lock(&api.cancelled).len(), 1, "the unfair intent is cancelled");
        assert!(sync::lock(&book.payments).is_empty(), "nothing is paid");
        assert_eq!(book.prepays.load(Ordering::SeqCst), 0, "the attempt holds off");
        assert!(runner.held_off());
    }

    // The ledger-service admits a top-up to the pin only with the purchase memo: pay no other
    #[tokio::test]
    async fn an_intent_without_the_purchase_memo_prefix_is_cancelled_unpaid() {
        let book = Arc::new(book("20", "20"));
        let mut fake = FakePurchases::new(Decimal::from(175));
        fake.memo_prefix = "traffic:";
        let api = Arc::new(fake);
        let runner = token_runner(&book, &api).await;
        runner.maybe_topup().await;
        assert_eq!(*sync::lock(&api.created), vec!["TUSDT"]);
        assert_eq!(sync::lock(&api.cancelled).len(), 1, "the intent is cancelled");
        assert!(sync::lock(&book.payments).is_empty(), "nothing is paid");
        assert!(runner.held_off());
        let paid = Arc::new(self::book("20", "20"));
        let api = Arc::new(FakePurchases::new(Decimal::from(175)));
        token_runner(&paid, &api).await.maybe_topup().await;
        let memo = sync::lock(&paid.signed)[0].clone();
        assert!(agent_control::is_traffic_purchase_memo(&memo), "{memo}");
        let payment = sync::lock(&paid.payments)[0].clone();
        let Some(Params::TransferCip56(p)) = payment.request.params else { panic!("a CIP-56 transfer") };
        assert!(p.direct_only && p.reference.as_deref() == Some(memo.as_str()));
        forget_memos(&paid);
    }

    #[tokio::test]
    async fn a_pin_or_instrument_mismatch_is_refused() {
        for (recipient, wire) in [("other::1220ff", TUSDT_WIRE), (PIN, ("tusdt-wire", "other-registry::1220ff"))] {
            let book = Arc::new(book("20", "20"));
            let mut fake = FakePurchases::new(Decimal::from(175));
            fake.recipient = recipient.into();
            fake.catalog.insert("TUSDT", wire);
            let api = Arc::new(fake);
            token_runner(&book, &api).await.maybe_topup().await;
            assert!(sync::lock(&book.payments).is_empty(), "{recipient} {wire:?}: nothing is paid");
            assert_eq!(sync::lock(&api.cancelled).len(), 1, "{recipient} {wire:?}");
            assert_eq!(book.prepays.load(Ordering::SeqCst), 0);
        }
    }

    /// Reads (maybe tops up) `times` times, `step` apart.
    async fn poll(runner: &TopupRunner, times: u32, step: Duration) {
        for _ in 0..times {
            tokio::time::advance(step).await;
            runner.maybe_topup().await;
        }
    }

    // A payment that committed is waited for until its purchase is credited, never paid again
    #[tokio::test(start_paused = true)]
    async fn a_committed_payment_waits_for_its_credit_and_is_never_paid_again() {
        let book = Arc::new(book("100", "0"));
        let api = Arc::new(FakePurchases::new(Decimal::from(175)));
        let runner = token_runner(&book, &api).await;
        runner.maybe_topup().await;
        assert_eq!(sync::lock(&api.created).len(), 1);
        let first = sync::lock(&book.signed)[0].clone();
        runner.maybe_topup().await;
        assert_eq!(api.gets.load(Ordering::SeqCst), 0, "the status is read at most every 30 s");
        tokio::time::advance(STATUS_POLL_EVERY).await;
        runner.maybe_topup().await;
        assert_eq!(api.gets.load(Ordering::SeqCst), 1);
        // The purchase expires and the credit is late: hours pass, nothing is paid again
        *sync::lock(&api.status) = "expired";
        poll(&runner, 6 * 12, Duration::from_secs(5 * 60)).await;
        assert_eq!(sync::lock(&api.created).len(), 1, "a committed payment is never paid again");
        assert!(crate::control::intent::is_open_traffic_purchase(&first));
        *sync::lock(&api.status) = "late";
        poll(&runner, 1, STATUS_POLL_EVERY).await;
        assert!(!crate::control::intent::is_open_traffic_purchase(&first), "a credited memo is forgotten");
        assert_eq!(sync::lock(&api.created).len(), 2, "the balance is still low: the next top-up runs");
        forget_memos(&book);
    }

    // An execute of unknown outcome is waited for 45 min, then its purchase decides
    #[tokio::test(start_paused = true)]
    async fn an_unknown_outcome_waits_then_reads_the_purchase_before_paying_again() {
        let book = Arc::new(FakeBook { ambiguous: true, ..book("100", "0") });
        let api = Arc::new(FakePurchases::new(Decimal::from(175)));
        let runner = token_runner(&book, &api).await;
        runner.maybe_topup().await;
        assert_eq!(sync::lock(&api.created).len(), 1);
        assert!(sync::lock(&api.cancelled).is_empty(), "an intent that may be paid is not cancelled");
        *sync::lock(&api.status) = "expired";
        let step = Duration::from_secs(60);
        let waits = u32::try_from(UNKNOWN_OUTCOME_WAIT.as_secs() / 60).unwrap() - 1;
        poll(&runner, waits, step).await;
        assert_eq!(sync::lock(&api.created).len(), 1, "still waiting at {waits} min");
        // At 45 min an unreadable status still holds the payment
        *sync::lock(&api.status) = "unavailable";
        poll(&runner, 2, step).await;
        assert_eq!(sync::lock(&api.created).len(), 1, "no status, no new payment");
        *sync::lock(&api.status) = "expired";
        poll(&runner, 1, step).await;
        assert_eq!(sync::lock(&api.created).len(), 2, "not credited after the wait: a new purchase");
        forget_memos(&book);
    }

    // The payment in flight is kept in the agent folder: a restart resumes the wait
    #[tokio::test(start_paused = true)]
    async fn a_restart_resumes_the_wait_from_the_payment_file() {
        let dir = std::env::temp_dir().join(format!("topup-payment-{}-{}", std::process::id(), NEXT_FAKE.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(PAYMENT_FILE);
        let book = Arc::new(book("100", "0"));
        let api = Arc::new(FakePurchases::new(Decimal::from(175)));
        token_runner(&book, &api).await.with_payment_file(file.clone()).maybe_topup().await;
        assert_eq!(sync::lock(&api.created).len(), 1);
        let saved = load_payment(&file).unwrap().expect("the payment is on file");
        assert_eq!((saved.token.as_str(), saved.token_amount), ("TUSDT", d("13.125")));
        assert_eq!(saved.update_id.as_deref(), Some("u-tusdt-wire"));
        assert_eq!(saved.memo, sync::lock(&book.signed)[0]);
        // The process restarts: the new runner waits for the same purchase
        let runner = token_runner(&book, &api).await.with_payment_file(file.clone());
        runner.maybe_topup().await;
        assert_eq!(api.gets.load(Ordering::SeqCst), 1, "the status is read at once after a restart");
        poll(&runner, 3, Duration::from_secs(30 * 60)).await;
        assert_eq!(sync::lock(&api.created).len(), 1, "nothing is paid again after the restart");
        *sync::lock(&api.status) = "credited";
        poll(&runner, 1, STATUS_POLL_EVERY).await;
        assert_eq!(sync::lock(&api.created).len(), 2, "credited: the next top-up runs");
        let next = load_payment(&file).unwrap().expect("the next payment is on file");
        assert_ne!(next.purchase_id, saved.purchase_id);
        // An unknown outcome on file waits its 45 min from when it started
        *sync::lock(&api.status) = "expired";
        let unknown = PaymentRecord { update_id: None, started_at_ms: clock::now_millis().saturating_sub(60_000), ..next.clone() };
        save_payment(&file, &unknown).unwrap();
        let runner = token_runner(&book, &api).await.with_payment_file(file.clone());
        poll(&runner, 40, Duration::from_secs(60)).await;
        assert_eq!(sync::lock(&api.created).len(), 2, "within its wait");
        poll(&runner, 6, Duration::from_secs(60)).await;
        assert_eq!(sync::lock(&api.created).len(), 3, "past its wait, not credited: a new purchase");
        // A payment refused before signing leaves no file
        let refused = Arc::new(FakeBook { pending_for: Some(TUSDT_WIRE.0), ..book_with_cc("100", "0", "0") });
        std::fs::remove_file(&file).unwrap();
        token_runner(&refused, &api).await.with_payment_file(file.clone()).maybe_topup().await;
        assert!(load_payment(&file).unwrap().is_none(), "nothing to wait for");
        forget_memos(&book);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // The pin is read per walk: a pin installed after start enables the stables
    #[tokio::test]
    async fn a_traffic_fee_pin_picked_up_after_start_enables_the_stables() {
        let book = Arc::new(book_with_cc("100", "0", "0"));
        let api = Arc::new(FakePurchases::new(Decimal::from(175)));
        let pin: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
        let source = Arc::clone(&pin);
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_usd_rate(d("0.075")).await;
        let runner = TopupRunner::over(book.clone(), "agent::1220ee".into(), Some(Decimal::from(50)), Some(Decimal::from(175)))
            .unwrap()
            .with_token_topups(
                vec![stable("TUSDT", TUSDT_WIRE), TopupToken::Cc],
                Some(StableTopups {
                    traffic_fee_party: Arc::new(move || sync::lock(&source).clone()),
                    purchases: api.clone(),
                    rates: lm,
                }),
            );
        runner.maybe_topup().await;
        assert!(sync::lock(&api.created).is_empty(), "no pin: the stable is skipped");
        *sync::lock(&pin) = Some(PIN.into());
        runner.maybe_topup().await;
        assert_eq!(*sync::lock(&api.created), vec!["TUSDT"], "the pin in force is used");
        let payment = sync::lock(&book.payments)[0].clone();
        assert_eq!(payment.check.receiver, PIN);
        forget_memos(&book);
    }

    // The pins in force win over the config; a malformed or blank one counts as unset
    #[test]
    fn the_traffic_fee_pin_in_force_falls_back_to_the_config() {
        assert_eq!(traffic_fee_pin(Some("fee-b::1220"), Some("fee-a::1220")).as_deref(), Some("fee-b::1220"));
        assert_eq!(traffic_fee_pin(None, Some("fee-a::1220")).as_deref(), Some("fee-a::1220"));
        assert_eq!(traffic_fee_pin(Some("  "), Some(" fee-a::1220 ")).as_deref(), Some("fee-a::1220"));
        assert_eq!(traffic_fee_pin(None, None), None);
    }

    #[test]
    fn a_payment_file_reads_back_and_refuses_what_it_did_not_write() {
        let dir = std::env::temp_dir().join(format!("topup-record-{}-{}", std::process::id(), NEXT_FAKE.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(PAYMENT_FILE);
        assert_eq!(load_payment(&file).unwrap(), None, "no file, nothing in flight");
        let record = PaymentRecord {
            purchase_id: "p-1".into(),
            memo: "agent-traffic:p-1".into(),
            token: "TUSDT".into(),
            token_amount: d("13.125"),
            update_id: None,
            started_at_ms: 1_764_100_000_000,
        };
        save_payment(&file, &record).unwrap();
        assert_eq!(load_payment(&file).unwrap(), Some(record.clone()));
        let committed = PaymentRecord { update_id: Some("u-1".into()), ..record.clone() };
        save_payment(&file, &committed).unwrap();
        assert_eq!(load_payment(&file).unwrap(), Some(committed.clone()));
        // A record that would not read back is never written
        let padded = PaymentRecord { purchase_id: " p-1".into(), ..record };
        assert!(save_payment(&file, &padded).is_err());
        assert_eq!(load_payment(&file).unwrap(), Some(committed));
        for bad in ["{}", "[]", "not json", r#"{"purchase_id":"","memo":"m","token":"T","token_amount":"1","update_id":null,"started_at_ms":1}"#,
            r#"{"purchase_id":"p","memo":"m","token":"T","token_amount":"1e3","update_id":null,"started_at_ms":1}"#,
            r#"{"purchase_id":"p","memo":"m","token":"T","token_amount":"1","update_id":"","started_at_ms":1}"#] {
            std::fs::write(&file, bad).unwrap();
            assert!(load_payment(&file).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_payment_signs_only_one_direct_holding_to_the_pin_with_the_memo() {
        let want = PaymentCheck {
            receiver: PIN.into(),
            instrument_id: TUSDT_WIRE.0.into(),
            instrument_admin: TUSDT_WIRE.1.into(),
            amount: d("13.125"),
            memo: "agent-traffic:p1".into(),
        };
        let flow = |kind, to: &str, amount: &str| ValueFlow {
            to: to.into(),
            instrument_admin: TUSDT_WIRE.1.into(),
            instrument_id: TUSDT_WIRE.0.into(),
            amount: d(amount),
            kind,
        };
        let facts = |outflows: Vec<ValueFlow>, memo: &str| -> Result<TxFacts, String> {
            Ok(TxFacts { outflows, memos: vec![memo.to_string()], ..TxFacts::default() })
        };
        let memo = "agent-traffic:p1";
        check_payment(&facts(vec![flow(FlowKind::Holding, PIN, "13.1250")], memo), &want).unwrap();
        let refused = [
            facts(vec![flow(FlowKind::PendingTransfer, PIN, "13.125")], memo),
            facts(vec![flow(FlowKind::Holding, "other::1", "13.125")], memo),
            facts(vec![flow(FlowKind::Holding, PIN, "13.124")], memo),
            facts(vec![flow(FlowKind::Holding, PIN, "13.125"), flow(FlowKind::Holding, PIN, "1")], memo),
            facts(Vec::new(), memo),
            facts(vec![flow(FlowKind::Holding, PIN, "13.125")], "agent-traffic:p2"),
            Err("unreadable".into()),
        ];
        for case in refused {
            assert!(check_payment(&case, &want).is_err(), "{case:?}");
        }
        let other_instrument = Ok(TxFacts {
            outflows: vec![ValueFlow { instrument_id: "USDCx".into(), ..flow(FlowKind::Holding, PIN, "13.125") }],
            memos: vec![memo.into()],
            ..TxFacts::default()
        });
        assert!(check_payment(&other_instrument, &want).is_err());
    }

    #[test]
    fn a_stable_amount_is_rounded_up_to_6_decimals() {
        assert_eq!(stable_amount(d("175"), d("0.075")), Some(d("13.125")));
        assert_eq!(stable_amount(d("175"), d("0.0746")), Some(d("13.055")));
        assert_eq!(stable_amount(d("1"), d("0.00000013")), Some(d("0.000001")));
        assert_eq!(stable_amount(d("3"), d("0.3333333")), Some(d("1")));
        assert_eq!(stable_amount(d("175"), Decimal::ZERO), None);
        assert_eq!(stable_amount(d("175"), d("-0.1")), None);
        assert_eq!(stable_amount(Decimal::MAX, Decimal::TEN), None);
    }

    #[test]
    fn top_up_tokens_resolve_through_the_registry_in_order() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.prepaid_traffic_topup_tokens = vec!["TUSDT".into(), "NOPE".into(), "CC".into()];
        config.instrument_wire_ids.insert("TUSDT".into(), TUSDT_WIRE.0.into());
        config.instrument_registries.insert("TUSDT".into(), TUSDT_WIRE.1.into());
        assert_eq!(topup_tokens(&config), vec![stable("TUSDT", TUSDT_WIRE), TopupToken::Cc]);
    }

    #[test]
    fn only_a_list_with_cc_counts_wallet_cc_as_fee_capacity() {
        assert!(moves_wallet_cc(&[stable("TUSDT", TUSDT_WIRE), TopupToken::Cc]));
        assert!(!moves_wallet_cc(&[stable("TUSDT", TUSDT_WIRE), stable("USDCx", USDCX_WIRE)]));
    }

    #[test]
    fn unlocked_amounts_fall_back_to_total_less_locked() {
        let b = |total: &str, locked: &str, unlocked: &str| TokenBalance {
            total_amount: total.into(),
            locked_amount: locked.into(),
            unlocked_amount: unlocked.into(),
            ..holding(TUSDT_WIRE, "0")
        };
        assert_eq!(unlocked_amount(&b("100", "30", "70")), Some(d("70")));
        assert_eq!(unlocked_amount(&b("100", "", "")), Some(d("100")));
        assert_eq!(unlocked_amount(&b("100", "30", "")), Some(d("70")));
        assert_eq!(unlocked_amount(&b("x", "", "")), None);
        let balances = [holding(TUSDT_WIRE, "5"), holding(("tusdt-wire", "other::1"), "50"), cc_holding("7")];
        assert_eq!(unlocked_of(&balances, TUSDT_WIRE.0, TUSDT_WIRE.1), d("5"), "registry and wire id both match");
        assert_eq!(unlocked_cc(&balances), d("7"));
    }

    #[tokio::test]
    async fn the_cli_purchase_pays_once_and_reads_the_credit() {
        let book = book("20", "0");
        let api = FakePurchases::new(Decimal::from(175));
        let buy = StablePurchase {
            token: "TUSDT",
            wire_id: TUSDT_WIRE.0,
            registry: TUSDT_WIRE.1,
            traffic_fee_party: PIN,
            credit_cc: Decimal::from(175),
            cc_usd_rate: d("0.075"),
        };
        let deadline = clock::deadline_after(Duration::from_secs(30));
        let bought = buy_traffic(&book, &api, "agent::1220ee", &buy, deadline).await.unwrap();
        assert_eq!((bought.update_id.as_str(), bought.token_amount), ("u-tusdt-wire", d("13.125")));
        assert_eq!(bought.memo, format!("agent-traffic:{}", bought.purchase_id));
        *sync::lock(&api.status) = "credited";
        let credited = wait_for_credit(&api, &bought.purchase_id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(credited.status, "credited");
        crate::control::intent::remove_open_traffic_purchase(&bought.memo);
        // A pending-transfer shape is never signed, and its intent is cancelled
        let pending = FakeBook { pending_for: Some(TUSDT_WIRE.0), ..self::book("20", "0") };
        let err = buy_traffic(&pending, &api, "agent::1220ee", &buy, deadline).await.unwrap_err();
        assert!(format!("{err:#}").contains("was not made"), "{err:#}");
        assert_eq!(sync::lock(&api.cancelled).len(), 1);
        assert!(sync::lock(&pending.signed).is_empty());
    }

    #[test]
    fn spawning_outside_a_runtime_is_an_error() {
        let runner = {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async { runner() })
        };
        assert!(Arc::new(runner).spawn(Shutdown::new()).is_err());
    }
}
