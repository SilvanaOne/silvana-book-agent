//! Direct commands: `control execute`, `control commands` and the daemon's executor. A
//! command gets at most one signed attempt, bound to its transaction before the signature.

#![cfg_attr(
    not(test),
    allow(renamed_and_removed_lints),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::unchecked_duration_subtraction,
        clippy::arithmetic_side_effects,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::dbg_macro,
        clippy::disallowed_methods
    ),
    warn(renamed_and_removed_lints)
)]

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_control::binding::BindingError;
use agent_control::{
    Amount, AnyReceipt, CodeSource, Command, CommandError, CommandOp, ControlState, EvalContext,
    Hash32, InstrumentId, InstrumentRef, PartyId, ProofEnvelope, Receipt, ReceiptOutcome,
    ReceiptSubject, SwapReceipt, SwapResult, Token, UsageBucket, UsageDelta, UsageRecord,
    UsageTotals, UtcTime, Uuid7, canton_coin, precheck, verify_command,
};
use agent_logic::config::BaseConfig;
use agent_logic::control_store::{
    CommandAttempt, CommandBook, CommandEnd, CommandRecord, CommandStatus, CommandUsage,
    CommandVerdict, ControlLock, ControlStore, SpendRecord, StoredReceipt,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use orderbook_proto::ledger::prepare_transaction_request::Params;
use orderbook_proto::ledger::{
    ControllerCommand, ExecuteTransactionResponse, PrepareTransactionRequest, TransactionOperation,
    TransferCcParams, TransferCip56Params,
};
use orderbook_proto::orderbook::PendingCommand;
use tracing::{debug, error, info, warn};
use tx_verifier::OperationExpectation;

use super::ControlContext;
use super::intent::{self, ContentRefusal, DirectedSignature, OwnRequest, SignatureRefusal};
use super::poller::CommandExecutor;
use super::prompt::{Prompter, Yes, confirm_typed};
use super::rpc::{ControlRpc, RpcError, document_hash, refusal_code};
use super::sync;
use crate::ledger_client::{
    ATOMIC_VERIFICATION_REJECTED, CommandSearch, DAppProviderClient, NO_HASH_TO_SIGN, PreSign,
    PreSignContext, VERIFICATION_REJECTED,
};

/// How long past a command's expiry its absence on the ledger proves it never committed.
pub const RECOVERY_GRACE_SECS: i64 = 600;
/// The code of a command on the Agent page: `CMD-<command id>`.
pub const CODE_PREFIX: &str = "CMD-";
/// Bound on one control rpc call made here.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on connecting to the ledger-service.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on reading the ledger end.
const LEDGER_END_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on one directed submission: prepare, the binding, the signature and the execute.
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(300);
/// Bound on one ledger scan for a Canton command id: above the client's own bounds, so a scan
/// cut short comes back with how far it got.
fn scan_timeout(config: &BaseConfig) -> Duration {
    let secs = config.request_timeout_secs;
    CONNECT_TIMEOUT
        .saturating_add(crate::ledger_client::call_wait(secs))
        .saturating_add(crate::ledger_client::stream_wait(secs))
}
/// Bound on re-reading the control log before a command is verified.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Open records settled per round of the daemon.
const MAX_OPEN_PER_ROUND: usize = 16;
/// Key prefix of the uses commands record in the spend counters.
const USAGE_KEY_PREFIX: &str = "command:";
/// Ledger codes answered with a signed refusal.
const LEDGER_REFUSALS: std::ops::RangeInclusive<u16> = 4110..=4131;
/// Ledger codes raised as a critical alert, then answered with a signed refusal.
const LEDGER_CRITICAL: [u16; 3] = [4102, 4104, 4105];
/// The name a receipt's ledger code goes by here.
const LEDGER_REFUSAL: &str = "ledger_refusal";
/// The ledger-service's copy of the control log lacks the head the token claims.
const LOG_BEHIND: u16 = 4103;
/// The ledger-service refused the token's control claim.
const TOKEN_REFUSED: u16 = 4106;
/// Bound on syncing the control log after [`LOG_BEHIND`].
const SYNC_TIMEOUT: Duration = Duration::from_secs(180);

/// The control rpc calls a command needs.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait CommandRpc: Send + Sync {
    /// Now, while this clock agrees with the rpc's.
    fn checked_now(&self) -> Result<UtcTime>;
    async fn pending_commands(&self) -> Result<Vec<PendingCommand>, RpcError>;
    async fn post_receipt(&self, receipt: Vec<u8>, signature: [u8; 64]) -> Result<(), RpcError>;
    /// Uploads what the rpc's copy of the control log lacks and settles a pending entry.
    async fn sync_log(&self, ctx: &ControlContext) -> Result<()>;
}

#[async_trait::async_trait]
impl CommandRpc for ControlRpc {
    fn checked_now(&self) -> Result<UtcTime> {
        ControlRpc::checked_now(self)
    }

    async fn pending_commands(&self) -> Result<Vec<PendingCommand>, RpcError> {
        self.pending().await.map(|listing| listing.commands)
    }

    async fn post_receipt(&self, receipt: Vec<u8>, signature: [u8; 64]) -> Result<(), RpcError> {
        ControlRpc::post_receipt(self, receipt, signature)
            .await
            .map(|_| ())
    }

    async fn sync_log(&self, ctx: &ControlContext) -> Result<()> {
        let mut account: Vec<u8> = Vec::new();
        let synced = sync::run_sync(ctx, self, &mut account).await;
        for line in String::from_utf8_lossy(&account).lines() {
            if !line.trim().is_empty() {
                info!("agent control: {line}");
            }
        }
        synced
    }
}

/// The ledger calls a directed command needs.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait DirectedLedger: Send {
    async fn ledger_end(&mut self) -> Result<i64>;
    /// One attempt; `bind` runs right before its signature.
    async fn submit_directed(
        &mut self,
        req: PrepareTransactionRequest,
        expectation: &OperationExpectation,
        command: &Command,
        bind: PreSign<'_>,
    ) -> Result<ExecuteTransactionResponse>;
    async fn find_command(&mut self, command_id: &str, from_offset: i64) -> CommandSearch;
    /// The RFQ venue and atomic ledger a swap uses, if this ledger has them.
    fn swaps(&mut self) -> Option<&mut dyn swap::SwapDesk> {
        None
    }
    /// Drops the connected clients, so the next call mints a new ledger token.
    fn remint(&mut self);
}

/// The agent's ledger client, connected on first use.
pub struct LazyLedger {
    config: BaseConfig,
    client: Option<DAppProviderClient>,
    desk: swap::LiveDesk,
}

impl LazyLedger {
    pub fn new(config: BaseConfig) -> Self {
        let desk = swap::LiveDesk::new(config.clone());
        Self {
            config,
            client: None,
            desk,
        }
    }

    async fn client(&mut self) -> Result<&mut DAppProviderClient> {
        if self.client.is_none() {
            let c = &self.config;
            let connect = DAppProviderClient::new(
                &c.orderbook_grpc_url,
                &c.party_id,
                &c.role,
                &c.private_key,
                c.token_ttl_secs,
                Some(c.node_name.as_str()),
                &c.ledger_service_public_key,
                Some(c.connection_timeout_secs),
                Some(c.request_timeout_secs),
            );
            let client = tokio::time::timeout(CONNECT_TIMEOUT, connect)
                .await
                .map_err(|_| {
                    anyhow!("the ledger-service did not answer within {CONNECT_TIMEOUT:?}")
                })??;
            self.client = Some(client);
        }
        self.client
            .as_mut()
            .ok_or_else(|| anyhow!("the ledger client is not connected"))
    }
}

#[async_trait::async_trait]
impl DirectedLedger for LazyLedger {
    async fn ledger_end(&mut self) -> Result<i64> {
        self.client().await?.get_ledger_end().await
    }

    async fn submit_directed(
        &mut self,
        req: PrepareTransactionRequest,
        expectation: &OperationExpectation,
        command: &Command,
        bind: PreSign<'_>,
    ) -> Result<ExecuteTransactionResponse> {
        self.client()
            .await?
            .submit_directed(req, expectation, command, bind)
            .await
    }

    async fn find_command(&mut self, command_id: &str, from_offset: i64) -> CommandSearch {
        match self.client().await {
            Ok(client) => client.find_command(command_id, from_offset).await,
            Err(e) => CommandSearch::Unknown(format!("{e:#}")),
        }
    }

    fn swaps(&mut self) -> Option<&mut dyn swap::SwapDesk> {
        Some(&mut self.desk)
    }

    fn remint(&mut self) {
        self.client = None;
        swap::SwapDesk::remint(&mut self.desk);
    }
}

/// What a command run uses.
pub struct Executor<'a> {
    pub ctx: &'a ControlContext,
    pub rpc: &'a dyn CommandRpc,
    pub ledger: &'a mut dyn DirectedLedger,
}

impl<'a> Executor<'a> {
    fn answers(&self) -> Answers<'a> {
        Answers {
            ctx: self.ctx,
            rpc: self.rpc,
        }
    }
}

/// What signing and delivering a receipt uses.
#[derive(Clone, Copy)]
struct Answers<'a> {
    ctx: &'a ControlContext,
    rpc: &'a dyn CommandRpc,
}

/// How a pending command is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `control execute`: shown, then confirmed by the typed phrase.
    Confirm,
    /// The daemon with `auto_execute_commands`: no prompt.
    Automatic,
    /// The daemon otherwise: only a command that can never run is answered.
    Waiting,
}

/// What became of one command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handled {
    Executed {
        update_id: String,
        delivered: bool,
    },
    Refused {
        code: u16,
        name: &'static str,
        delivered: bool,
    },
    Failed {
        code: u16,
        name: &'static str,
        delivered: bool,
    },
    /// Signed; the ledger does not show its outcome yet. It is never signed again.
    Unsettled(String),
    /// Valid; it waits for `control execute`.
    Waiting,
    /// Nothing was signed; it may run later.
    NotYet(String),
    /// Settled earlier; its receipt was delivered again when the rpc lacked it.
    Settled {
        status: CommandStatus,
        delivered: bool,
    },
}

/// A command as the rpc listed it; nothing in it is trusted before verification.
#[derive(Clone, Debug)]
struct Pending {
    id: Uuid7,
    bytes: Vec<u8>,
    proof: Vec<u8>,
    decoded: Option<Command>,
    hash: Hash32,
}

impl Pending {
    fn of(item: &PendingCommand) -> Result<Self> {
        let id = Uuid7::parse(&item.command_id).map_err(|_| {
            anyhow!(
                "the rpc lists a command under the invalid id {:?}",
                item.command_id
            )
        })?;
        let decoded = Command::decode(&item.command).ok();
        if let Some(command) = &decoded {
            ensure!(
                command.command_id == id,
                "the rpc lists command {id} with the bytes of command {}",
                command.command_id
            );
        }
        Ok(Self {
            id,
            bytes: item.command.clone(),
            proof: item.proof.clone(),
            decoded,
            hash: document_hash(&item.command),
        })
    }

    fn expires_at(&self, now: UtcTime) -> i64 {
        self.decoded
            .as_ref()
            .map_or(now.unix_secs(), |c| c.expires_at.unix_secs())
    }
}

/// A verified command, ready for its attempt, with what the operator confirms.
#[derive(Clone)]
pub struct Ready {
    pending: Pending,
    command: Command,
    fingerprint: String,
    label: String,
    /// Canton Coin as this agent's DSO config names it.
    cc: Option<InstrumentRef>,
}

impl Ready {
    fn asset(&self) -> String {
        match &self.command.op {
            CommandOp::Swap { sell, .. } if self.cc.as_ref() == Some(sell) => "CC".to_string(),
            op => asset_of(op),
        }
    }

    fn is_swap(&self) -> bool {
        matches!(self.command.op, CommandOp::Swap { .. })
    }

    /// The typed confirmation: `transfer <amount> <asset>`, or `swap <amount> <asset>` for
    /// a swap of that amount of the sold asset.
    pub fn phrase(&self) -> String {
        let verb = if self.is_swap() { "swap" } else { "transfer" };
        format!("{verb} {} {}", self.command.op.amount(), self.asset())
    }

    fn write_summary(&self, out: &mut dyn Write) {
        let command = &self.command;
        let _ = writeln!(
            out,
            "Command {CODE_PREFIX}{} ({})",
            command.command_id,
            command.op.name()
        );
        match &command.op {
            CommandOp::TransferCc {
                receiver, amount, ..
            } => {
                let _ = writeln!(out, "  Asset:      CC (Canton Coin)");
                let _ = writeln!(out, "  Amount:     {amount}");
                let _ = writeln!(out, "  Recipient:  {receiver}");
            }
            CommandOp::TransferToken {
                receiver,
                instrument,
                amount,
                ..
            } => {
                let _ = writeln!(
                    out,
                    "  Asset:      {} (registry {})",
                    instrument.id, instrument.admin
                );
                let _ = writeln!(out, "  Amount:     {amount}");
                let _ = writeln!(out, "  Recipient:  {receiver}");
            }
            CommandOp::Swap {
                sell,
                sell_amount,
                buy,
                min_receive,
                max_fee_cc,
            } => {
                let net = if self.cc.as_ref() == Some(buy) {
                    ", net of the CC fee"
                } else {
                    ""
                };
                let _ = writeln!(
                    out,
                    "  Sell:       {sell_amount} {} (registry {})",
                    sell.id, sell.admin
                );
                let _ = writeln!(out, "  Buy:        {} (registry {})", buy.id, buy.admin);
                let _ = writeln!(out, "  At least:   {min_receive} {}{net}", buy.id);
                let _ = writeln!(out, "  Max fee:    {max_fee_cc} CC");
                let _ = writeln!(out, "  Through:    RFQ V2 quotes, one atomic settle");
            }
            CommandOp::UsdcxBurn { .. } => {}
        }
        let _ = writeln!(out, "  Signed by:  {} \"{}\"", self.fingerprint, self.label);
        let _ = writeln!(out, "  Expires:    {}", command.expires_at);
        let _ = writeln!(out, "  Summary:    {}", command.summary.as_str());
    }
}

/// What a listed command comes to before its attempt.
enum Admission {
    Done(Handled),
    Ready(Box<Ready>),
}

/// How the one submission of an attempt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Submitted {
    Executed(String),
    /// Refused before the signature, with the answer to sign now.
    Answered {
        refused: bool,
        code: u16,
        name: &'static str,
        from_ledger: bool,
    },
    /// Nothing was signed; the command may run again.
    Again(String),
    /// Signed; the outcome is open (with the ledger's answer, if it gave one).
    Open {
        verdict: Option<CommandVerdict>,
        why: String,
    },
}

fn asset_of(op: &CommandOp) -> String {
    match op {
        CommandOp::TransferCc { .. } => "CC".to_string(),
        CommandOp::TransferToken { instrument, .. } => instrument.id.as_str().to_string(),
        CommandOp::UsdcxBurn { .. } => "USDCx".to_string(),
        CommandOp::Swap { sell, .. } => sell.id.as_str().to_string(),
    }
}

/// `CMD-<lowercase uuid7>`, exactly.
pub fn parse_code(code: &str) -> Result<Uuid7> {
    let code = code.trim();
    let id = code
        .strip_prefix(CODE_PREFIX)
        .and_then(|rest| Uuid7::parse(rest).ok())
        .ok_or_else(|| {
            anyhow!("--code must be {CODE_PREFIX}<command id> as the Agent page shows it")
        })?;
    ensure!(
        code == format!("{CODE_PREFIX}{id}"),
        "--code must be {CODE_PREFIX}<command id> as the Agent page shows it"
    );
    Ok(id)
}

/// Canton Coin as this agent's own DSO config names it.
fn canton_coin_of(config: &BaseConfig) -> Result<InstrumentRef> {
    let dso = PartyId::parse(config.dso_party.trim()).map_err(|_| {
        anyhow!(
            "DSO={} is not a party id control accepts: a direct command needs it",
            config.dso_party
        )
    })?;
    canton_coin(&dso).map_err(|_| anyhow!("Canton Coin cannot be named under DSO={dso}"))
}

/// A refusal that holds whatever happens to the log or the clock.
fn never_valid(e: &CommandError) -> bool {
    matches!(
        e,
        CommandError::Canon(_)
            | CommandError::WrongNetwork
            | CommandError::WrongParty
            | CommandError::UnknownController
            | CommandError::StaleLogHead
            | CommandError::Expired
            | CommandError::Proof(_)
            | CommandError::TimeOverflow
    )
}

/// Re-reads the control log, then takes its lock with the state it holds.
async fn locked_state(ctx: &ControlContext) -> Result<(ControlLock, ControlState)> {
    match tokio::time::timeout(
        RELOAD_TIMEOUT,
        ctx.guard.reload_from(&ctx.store, ctx.marker.as_ref()),
    )
    .await
    {
        Ok(reloaded) => {
            reloaded.context("reloading the control state")?;
        }
        Err(_) => bail!("the control log was not re-read within {RELOAD_TIMEOUT:?}"),
    }
    let lock = ctx.store.lock().await?;
    let stored = ctx.store.load(&lock)?;
    let state = sync::state_of(ctx, stored.as_ref())?;
    ensure!(
        state.is_linked(),
        "this agent is not linked: no command applies to it"
    );
    ensure!(
        ctx.guard.head() == state.head(),
        "the control state in force differs from the store: retry"
    );
    Ok((lock, state))
}

/// The command's record, read under `lock`.
fn record_of(store: &ControlStore, lock: &ControlLock, id: &str) -> Result<Option<CommandRecord>> {
    Ok(store.commands(lock)?.get(id).cloned())
}

/// The agent's receipt of `end` for command `id`, signed; an executed swap's is v2.
#[allow(clippy::too_many_arguments)]
fn sign_receipt(
    ctx: &ControlContext,
    state: &ControlState,
    id: &Uuid7,
    hash: Hash32,
    end: &CommandEnd,
    from_ledger: bool,
    now: UtcTime,
    swap: Option<SwapResult>,
) -> Result<(Vec<u8>, [u8; 64])> {
    let source = Some(if from_ledger {
        CodeSource::Ledger
    } else {
        CodeSource::Agent
    });
    let (outcome, update_id, reason_code, code_source) = match end {
        CommandEnd::Executed { update_id } => {
            let token = Token::parse(update_id)
                .map_err(|_| anyhow!("update id {update_id:?} does not fit a receipt"))?;
            (ReceiptOutcome::Executed, Some(token), None, None)
        }
        CommandEnd::Refused { code } => (
            ReceiptOutcome::Refused,
            None,
            Some(u64::from(*code)),
            source,
        ),
        CommandEnd::Failed { code } => {
            (ReceiptOutcome::Failed, None, Some(u64::from(*code)), source)
        }
    };
    let receipt = Receipt {
        network: ctx.network,
        agent_party: ctx.party.clone(),
        agent_key: ctx.signer.public_key(),
        subject: ReceiptSubject::Command(id.clone()),
        subject_hash: hash,
        outcome,
        update_id,
        reason_code,
        code_source,
        log_seq: state.head().map_or(0, |head| head.seq),
        issued_at: now,
    };
    if let (Some(result), ReceiptOutcome::Executed) = (swap, receipt.outcome) {
        let receipt = AnyReceipt::V2(SwapReceipt { receipt, result });
        let bytes = receipt
            .encode()
            .map_err(|e| anyhow!("the receipt does not encode ({})", e.code_name()))?;
        let signature = ctx.signer.sign_any_receipt(&receipt)?;
        return Ok((bytes, signature));
    }
    let bytes = receipt
        .encode()
        .map_err(|e| anyhow!("the receipt does not encode ({})", e.code_name()))?;
    let signature = ctx.signer.sign_receipt(&receipt)?;
    Ok((bytes, signature))
}

/// The receipt result of an executed swap, from the quote its attempt recorded; none for
/// another command. A swap without one a receipt can carry is an error: it never gets v1.
fn swap_result_of(record: &CommandRecord) -> Result<Option<SwapResult>> {
    let recorded = record.attempt.as_ref().and_then(|a| a.swap.as_ref());
    let is_swap = recorded.is_some()
        || BASE64
            .decode(&record.canonical_b64)
            .ok()
            .and_then(|bytes| Command::decode(&bytes).ok())
            .is_some_and(|c| matches!(c.op, CommandOp::Swap { .. }));
    if !is_swap {
        return Ok(None);
    }
    let recorded = recorded.ok_or_else(|| anyhow!("the swap recorded no quote"))?;
    swap::result_of(recorded).map(Some)
}

/// [`swap_result_of`] for the record of command `id`, read under `lock`.
fn swap_result(store: &ControlStore, lock: &ControlLock, id: &str) -> Result<Option<SwapResult>> {
    match record_of(store, lock, id)? {
        Some(record) => swap_result_of(&record),
        None => Ok(None),
    }
}

/// An executed swap whose v2 receipt cannot be built from its recorded attempt: its uses
/// count, a critical alert is raised, and it stays executing, reconciled and never signed again.
async fn withhold(
    desk: Answers<'_>,
    lock: &ControlLock,
    id: &Uuid7,
    update_id: &str,
) -> Result<Option<Handled>> {
    let Some(record) = record_of(&desk.ctx.store, lock, id.as_str())? else {
        return Ok(None);
    };
    let Err(e) = swap_result_of(&record) else {
        return Ok(None);
    };
    // Counted at the attempt's own time: a later round keeps the key, and the uses age out.
    if let Some(attempt) = &record.attempt {
        count_uses(desk, lock, id, attempt.started_at).await?;
    }
    let text = format!(
        "executed as update {update_id}, but its v2 receipt cannot be built ({e:#}); no receipt is signed and it stays executing"
    );
    error!("agent control: CRITICAL: command {id}: {text}");
    agent_logic::error_reporter::ErrorEventBuilder::new("CONTROL_COMMAND_CRITICAL", text)
        .severity("critical")
        .module("control.commands")
        .party(desk.ctx.party.as_str())
        .command_id(format!("{}{id}", agent_control::CANTON_COMMAND_PREFIX))
        .send();
    Ok(Some(Handled::Unsettled(format!(
        "command {id} executed as update {update_id}, but its receipt cannot be built ({e:#}); it is reconciled again and never signed again"
    ))))
}

/// Posts a receipt; true once the rpc took it. A failure leaves it for the next round.
async fn deliver(desk: Answers<'_>, id: &str, bytes: Vec<u8>, signature: [u8; 64]) -> bool {
    match tokio::time::timeout(RPC_TIMEOUT, desk.rpc.post_receipt(bytes, signature)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            warn!(
                "agent control: the receipt of command {id} is signed but not delivered ({e}); it is posted again later"
            );
            return false;
        }
        Err(_) => {
            warn!(
                "agent control: the receipt of command {id} was not delivered within {RPC_TIMEOUT:?}; it is posted again later"
            );
            return false;
        }
    }
    let marked = async {
        let lock = desk.ctx.store.lock().await?;
        let at = agent_logic::clock::now_secs_i64();
        desk.ctx
            .store
            .update_commands(&lock, at, |book| book.mark_receipt_posted(id, at))
    }
    .await;
    if let Err(e) = marked {
        warn!("agent control: the receipt of command {id} was delivered but not marked: {e:#}");
    }
    true
}

/// Delivers the stored receipt of a settled record again.
async fn redeliver(desk: Answers<'_>, record: &CommandRecord) -> Handled {
    let delivered = match record.receipt.as_ref().map(StoredReceipt::decode) {
        Some(Ok((bytes, signature))) => deliver(desk, &record.command_id, bytes, signature).await,
        Some(Err(e)) => {
            warn!(
                "agent control: the stored receipt of command {} is unreadable: {e:#}",
                record.command_id
            );
            false
        }
        None => false,
    };
    Handled::Settled {
        status: record.status,
        delivered,
    }
}

/// The spend-counter records of an executed command's uses.
fn spends_of(id: &str, usage: &[CommandUsage], at: i64) -> Vec<SpendRecord> {
    usage
        .iter()
        .enumerate()
        .map(|(i, u)| SpendRecord {
            key: format!("{USAGE_KEY_PREFIX}{id}:{i}:{}", u.bucket),
            instrument: format!("{}/{}", u.instrument_admin, u.instrument_id),
            amount: u.amount.clone(),
            at,
        })
        .collect()
}

/// A use as the evaluator takes it; another writer's record is skipped.
fn usage_record(spend: &SpendRecord) -> Result<Option<UsageRecord>> {
    if !spend.key.starts_with(USAGE_KEY_PREFIX) {
        return Ok(None);
    }
    let bad = || {
        anyhow!(
            "the spend counters hold an unreadable record {:?}",
            spend.key
        )
    };
    let bucket = spend
        .key
        .rsplit(':')
        .next()
        .and_then(UsageBucket::parse)
        .ok_or_else(bad)?;
    let (admin, id) = spend.instrument.split_once('/').ok_or_else(bad)?;
    let instrument = InstrumentRef {
        admin: PartyId::parse(admin).map_err(|_| bad())?,
        id: InstrumentId::parse(id).map_err(|_| bad())?,
    };
    let amount = Amount::parse(&spend.amount).map_err(|_| bad())?;
    let at = UtcTime::from_unix_secs(spend.at).ok_or_else(bad)?;
    Ok(Some(UsageRecord {
        bucket,
        instrument,
        amount,
        at,
    }))
}

/// The uses of the last day the caps count, from the spend counters.
async fn usage_totals(store: &ControlStore, now: UtcTime) -> Result<UsageTotals> {
    let spends = store
        .update_counters(now.unix_secs(), |book| Ok(book.spends().to_vec()))
        .await?;
    let mut records = Vec::new();
    for spend in &spends {
        if let Some(record) = usage_record(spend)? {
            records.push(record);
        }
    }
    Ok(UsageTotals::from_records(&records, now))
}

/// Counts an executed command's uses against the caps, under `lock`; counted again, they
/// keep their keys.
async fn count_uses(desk: Answers<'_>, lock: &ControlLock, id: &Uuid7, at: i64) -> Result<()> {
    let usage = record_of(&desk.ctx.store, lock, id.as_str())?
        .and_then(|r| r.attempt)
        .map(|a| a.usage)
        .unwrap_or_default();
    if usage.is_empty() {
        return Ok(());
    }
    let spends = spends_of(id.as_str(), &usage, at);
    let counted = desk
        .ctx
        .store
        .update_counters(at, move |book| {
            for spend in spends {
                book.record(spend)?;
            }
            Ok(())
        })
        .await;
    if let Err(e) = counted {
        error!("agent control: command {id} executed but its use was not counted: {e:#}");
    }
    Ok(())
}

fn usage_entries(usage: &[UsageDelta]) -> Vec<CommandUsage> {
    usage
        .iter()
        .map(|u| CommandUsage {
            bucket: u.bucket.as_str().to_string(),
            instrument_admin: u.instrument.admin.as_str().to_string(),
            instrument_id: u.instrument.id.as_str().to_string(),
            amount: u.amount.to_string(),
        })
        .collect()
}

/// Signs the receipt of `end`, settles the record under `lock`, then delivers the receipt;
/// an execution's uses then count against the caps.
async fn settle(
    desk: Answers<'_>,
    lock: ControlLock,
    state: &ControlState,
    id: &Uuid7,
    hash: Hash32,
    end: CommandEnd,
    from_ledger: bool,
) -> Result<bool> {
    let now = desk.rpc.checked_now()?;
    let swap = match &end {
        CommandEnd::Executed { .. } => swap_result(&desk.ctx.store, &lock, id.as_str())?,
        CommandEnd::Refused { .. } | CommandEnd::Failed { .. } => None,
    };
    let (bytes, signature) = sign_receipt(desk.ctx, state, id, hash, &end, from_ledger, now, swap)?;
    let at = now.unix_secs();
    let stored = StoredReceipt::new(&bytes, &signature);
    // Counted under the lock and before the record ends: no other run reads the totals
    // without these uses, and a crash in between counts them again under the same keys.
    if matches!(end, CommandEnd::Executed { .. }) {
        count_uses(desk, &lock, id, at).await?;
    }
    desk.ctx
        .store
        .update_commands(&lock, at, |book| book.finish(id.as_str(), &end, stored, at))?;
    drop(lock);
    Ok(deliver(desk, id.as_str(), bytes, signature).await)
}

/// A refusal under `lock`, signed and delivered; a record another run has signed or settled
/// meanwhile is never refused.
async fn refuse(
    desk: Answers<'_>,
    lock: ControlLock,
    state: &ControlState,
    pending: &Pending,
    code: u16,
    name: &'static str,
) -> Result<Handled> {
    match record_of(&desk.ctx.store, &lock, pending.id.as_str())? {
        Some(record) if record.status.is_final() => {
            drop(lock);
            return Ok(redeliver(desk, &record).await);
        }
        Some(record) if record.may_be_signed() => {
            drop(lock);
            warn!(
                "agent control: command {} is not refused ({name}, code {code}): another run may have signed it; it is reconciled",
                pending.id
            );
            return Ok(Handled::Unsettled(format!(
                "command {} may be signed by another run; it is reconciled, never refused",
                pending.id
            )));
        }
        _ => {}
    }
    info!(
        "agent control: command {} refused: {name} (code {code})",
        pending.id
    );
    let delivered = settle(
        desk,
        lock,
        state,
        &pending.id,
        pending.hash,
        CommandEnd::Refused { code },
        false,
    )
    .await?;
    Ok(Handled::Refused {
        code,
        name,
        delivered,
    })
}

/// Settles an executing record: an attempt that never reached its signature failed; a
/// signed one is looked for on the ledger from the offset read before it.
async fn reconcile(exec: &mut Executor<'_>, id: &str) -> Result<Handled> {
    let ctx = exec.ctx;
    let uuid = Uuid7::parse(id).map_err(|_| anyhow!("command id {id:?} is not a uuid7"))?;
    let (lock, state) = locked_state(ctx).await?;
    let record =
        record_of(&ctx.store, &lock, id)?.ok_or_else(|| anyhow!("command {id} is not recorded"))?;
    if record.status != CommandStatus::Executing {
        drop(lock);
        return Ok(Handled::Settled {
            status: record.status,
            delivered: record.receipt_posted,
        });
    }
    let attempt = record
        .attempt
        .clone()
        .ok_or_else(|| anyhow!("command {id} has no attempt"))?;
    let canonical = BASE64
        .decode(&record.canonical_b64)
        .context("the stored command is not base64")?;
    let hash = document_hash(&canonical);
    let quoting = Command::decode(&canonical).is_ok_and(|c| matches!(c.op, CommandOp::Swap { .. }));
    if !attempt.signing && quoting && exec.rpc.checked_now()?.unix_secs() <= record.expires_at {
        // A swap still taking quotes, here or in another run, has signed nothing; it ends by its expiry.
        drop(lock);
        return Ok(Handled::NotYet(format!(
            "swap {id} is taking quotes in another run, or a stopped run left it so; nothing is signed for it, and it fails once it expires"
        )));
    }
    if !attempt.signing {
        let failure = agent_control::CommandFailure::SubmitFailed;
        warn!(
            "agent control: command {id} stopped before its signature; it fails and is never signed"
        );
        let delivered = settle(
            exec.answers(),
            lock,
            &state,
            &uuid,
            hash,
            CommandEnd::Failed {
                code: failure.code(),
            },
            false,
        )
        .await?;
        return Ok(Handled::Failed {
            code: failure.code(),
            name: failure.code_name(),
            delivered,
        });
    }
    drop(lock);
    let bound = scan_timeout(&ctx.config);
    let search = tokio::time::timeout(
        bound,
        exec.ledger
            .find_command(&attempt.canton_command_id, attempt.ledger_offset),
    )
    .await
    .unwrap_or_else(|_| {
        CommandSearch::Unknown(format!("the ledger scan did not finish within {bound:?}"))
    });
    let now = exec.rpc.checked_now()?;
    let past_grace = now.unix_secs() > record.expires_at.saturating_add(RECOVERY_GRACE_SECS);
    let (end, from_ledger) = match search {
        CommandSearch::Found(update_id) => (CommandEnd::Executed { update_id }, false),
        CommandSearch::NotFound if past_grace => match &attempt.verdict {
            Some(v) if v.status == CommandStatus::Refused => {
                (CommandEnd::Refused { code: v.code }, v.from_ledger)
            }
            Some(v) => (CommandEnd::Failed { code: v.code }, v.from_ledger),
            None => (
                CommandEnd::Failed {
                    code: agent_control::CommandFailure::SubmitFailed.code(),
                },
                false,
            ),
        },
        CommandSearch::NotFound => {
            return Ok(Handled::Unsettled(format!(
                "{} is not on the ledger yet; it is settled once found, or after {}",
                attempt.canton_command_id,
                UtcTime::from_unix_secs(record.expires_at.saturating_add(RECOVERY_GRACE_SECS))
                    .map_or_else(|| "its expiry".to_string(), |at| at.to_string())
            )));
        }
        CommandSearch::Unknown(why) => {
            if past_grace {
                alert_unknown(ctx, id, &attempt, &why);
            }
            return Ok(Handled::Unsettled(why));
        }
        CommandSearch::Partial { scanned_to, why } => {
            // The next scan starts past what this one covered, never from the start again.
            let advanced = async {
                let lock = ctx.store.lock().await?;
                let at = now.unix_secs();
                ctx.store
                    .update_commands(&lock, at, |book| book.advance_scan(id, scanned_to, at))
            }
            .await;
            if let Err(e) = advanced {
                warn!("agent control: command {id}: the scan progress was not kept: {e:#}");
            }
            return Ok(Handled::Unsettled(format!(
                "{why}; {} is not up to offset {scanned_to}, and the next scan resumes there",
                attempt.canton_command_id
            )));
        }
    };
    let (lock, state) = locked_state(ctx).await?;
    match record_of(&ctx.store, &lock, id)? {
        Some(now) if now.status == CommandStatus::Executing => {}
        Some(settled) => {
            drop(lock);
            return Ok(Handled::Settled {
                status: settled.status,
                delivered: settled.receipt_posted,
            });
        }
        None => bail!("command {id} is not recorded"),
    }
    let withheld = match &end {
        CommandEnd::Executed { update_id } => {
            withhold(exec.answers(), &lock, &uuid, update_id).await?
        }
        CommandEnd::Refused { .. } | CommandEnd::Failed { .. } => None,
    };
    if let Some(withheld) = withheld {
        drop(lock);
        return Ok(withheld);
    }
    let delivered = settle(
        exec.answers(),
        lock,
        &state,
        &uuid,
        hash,
        end.clone(),
        from_ledger,
    )
    .await?;
    Ok(match end {
        CommandEnd::Executed { update_id } => Handled::Executed {
            update_id,
            delivered,
        },
        CommandEnd::Refused { code } => Handled::Refused {
            code,
            name: name_of(code, from_ledger),
            delivered,
        },
        CommandEnd::Failed { code } => Handled::Failed {
            code,
            name: name_of(code, from_ledger),
            delivered,
        },
    })
}

/// The name of a receipt code, as far as the agent knows it.
fn name_of(code: u16, from_ledger: bool) -> &'static str {
    if from_ledger {
        return LEDGER_REFUSAL;
    }
    agent_control::CommandFailure::from_code(u64::from(code)).map_or("refused", |f| f.code_name())
}

/// Everything before the attempt: a known command is settled, a new one verified at the
/// state under the lock; what cannot run is answered with a signed refusal.
async fn admit(exec: &mut Executor<'_>, item: &PendingCommand, mode: Mode) -> Result<Admission> {
    let ctx = exec.ctx;
    let pending = Pending::of(item)?;
    let id = pending.id.as_str().to_string();
    let known = {
        let lock = ctx.store.lock().await?;
        record_of(&ctx.store, &lock, &id)?
    };
    if let Some(record) = &known {
        ensure!(
            record.canonical_b64 == BASE64.encode(&pending.bytes),
            "the rpc lists command {id} with other bytes than this agent kept; it is not run"
        );
        if record.status.is_final() {
            return Ok(Admission::Done(redeliver(exec.answers(), record).await));
        }
        if record.status == CommandStatus::Executing {
            return reconcile(exec, &id).await.map(Admission::Done);
        }
    }
    let (lock, state) = locked_state(ctx).await?;
    let now = exec.rpc.checked_now()?;
    // Read again under this lock: another run may have taken the command meanwhile.
    let current = match record_of(&ctx.store, &lock, &id)? {
        Some(record) => record,
        None => {
            let at = now.unix_secs();
            ctx.store.update_commands(&lock, at, |book| {
                book.receive(&id, &pending.bytes, pending.expires_at(now), at)
                    .cloned()
            })?
        }
    };
    ensure!(
        current.canonical_b64 == BASE64.encode(&pending.bytes),
        "the rpc lists command {id} with other bytes than this agent kept; it is not run"
    );
    if current.status.is_final() {
        drop(lock);
        return Ok(Admission::Done(redeliver(exec.answers(), &current).await));
    }
    if current.status == CommandStatus::Executing {
        drop(lock);
        return reconcile(exec, &id).await.map(Admission::Done);
    }
    let envelope = match ProofEnvelope::decode(&pending.proof) {
        Ok(envelope) => envelope,
        Err(e) => {
            return refuse(
                exec.answers(),
                lock,
                &state,
                &pending,
                e.code(),
                e.code_name(),
            )
            .await
            .map(Admission::Done);
        }
    };
    let cc = canton_coin_of(&ctx.config).ok();
    let verified = match verify_command(&state, &pending.bytes, &envelope, now, cc.as_ref()) {
        Ok(verified) => verified,
        Err(CommandError::IssuedInFuture) => {
            return Ok(Admission::Done(Handled::NotYet(
                "the command is issued later than this clock allows".into(),
            )));
        }
        Err(e) if mode == Mode::Waiting && !never_valid(&e) => {
            return Ok(Admission::Done(Handled::Waiting));
        }
        Err(e) => {
            return refuse(
                exec.answers(),
                lock,
                &state,
                &pending,
                e.code(),
                e.code_name(),
            )
            .await
            .map(Admission::Done);
        }
    };
    let command = verified.command().clone();
    if matches!(command.op, CommandOp::UsdcxBurn { .. }) {
        // Until this agent runs burns, they are answered at once.
        let failure = BindingError::NotExecuted.failure();
        info!(
            "agent control: command {id} ({}) is not supported by this agent yet",
            command.op.name()
        );
        return refuse(
            exec.answers(),
            lock,
            &state,
            &pending,
            failure.code(),
            failure.code_name(),
        )
        .await
        .map(Admission::Done);
    }
    if mode == Mode::Waiting {
        return Ok(Admission::Done(Handled::Waiting));
    }
    let decision = precheck(
        state.rules_at(now).rules(),
        command.op.kind(),
        Some(&verified),
    );
    if !decision.allowed {
        let reason = decision.reason;
        return refuse(
            exec.answers(),
            lock,
            &state,
            &pending,
            reason.as_u16(),
            reason.code_name(),
        )
        .await
        .map(Admission::Done);
    }
    let label = state
        .controller(verified.controller_id())
        .map(|r| r.descriptor().label().as_str().to_string())
        .unwrap_or_default();
    drop(lock);
    let fingerprint = verified.controller_id().fingerprint();
    Ok(Admission::Ready(Box::new(Ready {
        fingerprint,
        label,
        pending,
        command,
        cc,
    })))
}

/// The prepare request and the expectation of a directed transfer, with its Canton command id.
fn request_for(
    command: &Command,
    pending: &Pending,
    envelope: &ProofEnvelope,
    agent: &str,
) -> Result<(PrepareTransactionRequest, OperationExpectation)> {
    let controller_command = Some(ControllerCommand {
        canonical: pending.bytes.clone(),
        controller_id: envelope.controller_id().to_string(),
        proof: pending.proof.clone(),
    });
    let canton_id = command.canton_command_id();
    match &command.op {
        CommandOp::TransferCc {
            receiver,
            amount,
            description,
            settlement_proposal_id,
        } => {
            let amount = amount.to_string();
            let expectation = OperationExpectation::TransferCc {
                sender_party: agent.to_string(),
                receiver_party: receiver.as_str().to_string(),
                amount: amount.clone(),
                command_id: canton_id.clone(),
                dso_party: None,
            };
            let params = Params::TransferCc(TransferCcParams {
                receiver_party: receiver.as_str().to_string(),
                amount,
                description: description.as_ref().map(|d| d.as_str().to_string()),
                command_id: canton_id,
                settlement_proposal_id: settlement_proposal_id
                    .as_ref()
                    .map(|s| s.as_str().to_string()),
                amulet_cids: Vec::new(),
            });
            let req = PrepareTransactionRequest {
                operation: TransactionOperation::TransferCc as i32,
                params: Some(params),
                request_signature: None,
                controller_command,
            };
            Ok((req, expectation))
        }
        CommandOp::TransferToken {
            receiver,
            instrument,
            amount,
            reference,
        } => {
            let amount = amount.to_string();
            let expectation = OperationExpectation::TransferCip56 {
                sender_party: agent.to_string(),
                receiver_party: receiver.as_str().to_string(),
                instrument_id: instrument.id.as_str().to_string(),
                instrument_admin: instrument.admin.as_str().to_string(),
                amount: amount.clone(),
                command_id: Some(canton_id),
                input_holding_cids: vec![],
            };
            let params = Params::TransferCip56(TransferCip56Params {
                instrument_id: instrument.id.as_str().to_string(),
                instrument_admin: instrument.admin.as_str().to_string(),
                receiver_party: receiver.as_str().to_string(),
                amount,
                reference: reference.as_ref().map(|r| r.as_str().to_string()),
                input_holding_cids: Vec::new(),
                max_input_holdings: None,
            });
            let req = PrepareTransactionRequest {
                operation: TransactionOperation::TransferCip56 as i32,
                params: Some(params),
                request_signature: None,
                controller_command,
            };
            Ok((req, expectation))
        }
        CommandOp::UsdcxBurn { .. } | CommandOp::Swap { .. } => {
            bail!("command {} is not a transfer", command.command_id)
        }
    }
}

/// A ledger-service code the agent answers with a signed refusal.
fn ledger_answer(text: &str) -> Option<u16> {
    refusal_code(text)
        .filter(|code| LEDGER_REFUSALS.contains(code) || LEDGER_CRITICAL.contains(code))
}

/// Refused locally before the signature: the content, the verifier or the local hash.
fn refused_content(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}");
    e.downcast_ref::<ContentRefusal>().is_some()
        || text.contains(VERIFICATION_REJECTED)
        || text.contains(ATOMIC_VERIFICATION_REJECTED)
        || text.contains(NO_HASH_TO_SIGN)
}

/// How the one submission ended, from its result and what the binding saw.
fn classify(
    result: Result<Result<ExecuteTransactionResponse>, tokio::time::error::Elapsed>,
    signed: bool,
    refusal: Option<SignatureRefusal>,
) -> Submitted {
    if let Some(refusal) = refusal {
        let (refused, code, name) = refusal.receipt_code();
        return Submitted::Answered {
            refused,
            code,
            name,
            from_ledger: false,
        };
    }
    let result = result.unwrap_or_else(|_| {
        Err(anyhow!(
            "the submission did not finish within {SUBMIT_TIMEOUT:?}"
        ))
    });
    match result {
        Ok(response) if signed && response.success && !response.update_id.is_empty() => {
            Submitted::Executed(response.update_id)
        }
        Ok(response) if signed => Submitted::Open {
            verdict: None,
            why: response
                .error_message
                .unwrap_or_else(|| "the execute reported no update".to_string()),
        },
        Ok(_) => Submitted::Again("the ledger-service answered without a signature".to_string()),
        Err(e) => {
            // The packages check of every signature, run before the binding
            if let Some(packages) = e.downcast_ref::<intent::PackageRefusal>().filter(|_| !signed) {
                let (refused, code, name) = packages.signature_refusal().receipt_code();
                return Submitted::Answered { refused, code, name, from_ledger: false };
            }
            let text = format!("{e:#}");
            let code = ledger_answer(&text);
            match (signed, code) {
                (false, Some(code)) => Submitted::Answered {
                    refused: true,
                    code,
                    name: LEDGER_REFUSAL,
                    from_ledger: true,
                },
                (false, None) if refused_content(&e) => {
                    let failure = agent_control::CommandFailure::ContentRefused;
                    Submitted::Answered {
                        refused: false,
                        code: failure.code(),
                        name: failure.code_name(),
                        from_ledger: false,
                    }
                }
                (false, None) => Submitted::Again(text),
                (true, code) => Submitted::Open {
                    verdict: code.map(|code| CommandVerdict {
                        status: CommandStatus::Refused,
                        code,
                        from_ledger: true,
                    }),
                    why: text,
                },
            }
        }
    }
}

/// Raises a critical alert about command `id`: a code the ledger-service must never give, or
/// content that is not what the command directs.
fn alert(ctx: &ControlContext, id: &str, code: u16, text: &str) {
    error!("agent control: CRITICAL: command {id}: {text} (code {code})");
    agent_logic::error_reporter::ErrorEventBuilder::new("CONTROL_COMMAND_CRITICAL", text)
        .severity("critical")
        .module("control.commands")
        .party(ctx.party.as_str())
        .command_id(format!("{}{id}", agent_control::CANTON_COMMAND_PREFIX))
        .error_code(code.to_string())
        .send();
}

/// Signed commands alerted as unknown past the grace in this run, so each is raised once.
static UNKNOWN_ALERTED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// True the first time `id` is alerted as unknown past the grace; the set is bounded.
fn first_unknown_alert(id: &str) -> bool {
    let mut alerted = agent_logic::sync::lock(&UNKNOWN_ALERTED);
    if alerted.len() >= 1024 {
        alerted.clear();
    }
    alerted.insert(id.to_string())
}

/// One critical alert for a signed command whose outcome the ledger still does not show past
/// its expiry and the grace, such as a scan from a pruned offset; it stays executing.
fn alert_unknown(ctx: &ControlContext, id: &str, attempt: &CommandAttempt, why: &str) {
    if !first_unknown_alert(id) {
        return;
    }
    let text = format!(
        "signed, and its outcome is still unknown past its expiry and the recovery grace: the scan for {} from offset {} does not conclude ({why}); it stays executing and is never signed again",
        attempt.canton_command_id, attempt.ledger_offset
    );
    error!("agent control: CRITICAL: command {id}: {text}");
    agent_logic::error_reporter::ErrorEventBuilder::new("CONTROL_COMMAND_CRITICAL", text)
        .severity("critical")
        .module("control.commands")
        .party(ctx.party.as_str())
        .command_id(format!("{}{id}", agent_control::CANTON_COMMAND_PREFIX))
        .send();
}

/// The alert an outcome calls for, if any.
fn alert_for(ctx: &ControlContext, id: &str, submitted: &Submitted, refusal: Option<&str>) {
    match submitted {
        Submitted::Answered {
            refused: false,
            code,
            ..
        } => {
            alert(
                ctx,
                id,
                *code,
                refusal.unwrap_or("the transaction is not the one the command directs"),
            );
        }
        Submitted::Answered {
            code,
            from_ledger: true,
            ..
        } if LEDGER_CRITICAL.contains(code) => {
            alert(ctx, id, *code, "the ledger-service gave a critical refusal");
        }
        Submitted::Open {
            verdict: Some(verdict),
            ..
        } if LEDGER_CRITICAL.contains(&verdict.code) => {
            alert(
                ctx,
                id,
                verdict.code,
                "the ledger-service gave a critical refusal after the signature",
            );
        }
        _ => {}
    }
}

/// The one attempt of a verified transfer: verified again under the lock, recorded, prepared
/// with its controller command, bound and signed once, then settled.
async fn attempt(exec: &mut Executor<'_>, ready: Ready) -> Result<Handled> {
    let ctx = exec.ctx;
    let rpc = exec.rpc;
    let Ready {
        pending, command, ..
    } = ready;
    let id = pending.id.as_str().to_string();
    let cc = canton_coin_of(&ctx.config)?;
    let platform = super::platform_parties()?;
    let pinned = platform.pinned().map_err(|e| anyhow!(e))?;
    let envelope = ProofEnvelope::decode(&pending.proof)
        .map_err(|e| anyhow!("the proof does not decode ({})", e.code_name()))?;
    let (req, mut expectation) = request_for(&command, &pending, &envelope, &ctx.config.party_id)?;
    if let OperationExpectation::TransferCc { dso_party, .. } = &mut expectation {
        *dso_party = crate::named_party(&ctx.config.dso_party);
    }
    let offset = match tokio::time::timeout(LEDGER_END_TIMEOUT, exec.ledger.ledger_end()).await {
        Ok(Ok(offset)) if offset > 0 => offset,
        Ok(Ok(_)) => return Ok(Handled::NotYet("the ledger end is unknown".into())),
        Ok(Err(e)) => {
            return Ok(Handled::NotYet(format!(
                "the ledger end could not be read: {e:#}"
            )));
        }
        Err(_) => {
            return Ok(Handled::NotYet(format!(
                "the ledger end was not read within {LEDGER_END_TIMEOUT:?}"
            )));
        }
    };
    let (lock, state) = locked_state(ctx).await?;
    let now = rpc.checked_now()?;
    match verify_command(&state, &pending.bytes, &envelope, now, Some(&cc)) {
        Ok(_) => {}
        Err(CommandError::IssuedInFuture) => {
            return Ok(Handled::NotYet(
                "the command is issued later than this clock allows".into(),
            ));
        }
        Err(e) => {
            return refuse(
                exec.answers(),
                lock,
                &state,
                &pending,
                e.code(),
                e.code_name(),
            )
            .await;
        }
    }
    let at = now.unix_secs();
    let started = CommandAttempt {
        canton_command_id: command.canton_command_id(),
        ledger_offset: offset,
        started_at: at,
        signing: false,
        usage: Vec::new(),
        verdict: None,
        swap: None,
    };
    ctx.store
        .update_commands(&lock, at, |book| book.begin(&id, started, at))?;
    let usage = match usage_totals(&ctx.store, now).await {
        Ok(usage) => usage,
        Err(e) => {
            ctx.store
                .update_commands(&lock, at, |book| book.release(&id, at))?;
            return Ok(Handled::NotYet(format!(
                "the spend counters could not be read: {e:#}"
            )));
        }
    };
    let mut refusal: Option<SignatureRefusal> = None;
    let mut signed = false;
    let result = {
        let (state, lock, envelope) = (&state, &lock, &envelope);
        let (party, agent, id) = (&ctx.party, ctx.config.party_id.as_str(), id.as_str());
        let params = req.params.clone();
        let mut bind = |pc: &PreSignContext<'_>| -> Result<()> {
            let now = rpc.checked_now()?;
            let eval = EvalContext {
                agent_party: party,
                cc: &cc,
                platform: &pinned,
                now,
                usage: &usage,
            };
            let directed = DirectedSignature {
                state,
                now,
                command: &pending.bytes,
                envelope,
                ctx: &eval,
            };
            let request = OwnRequest::Ledger(params.as_ref());
            let checked = intent::check_directed_transfer(
                agent,
                &platform,
                &expectation,
                request,
                pc.verification,
                pc.facts,
                pc.prepared_transaction,
                &directed,
            );
            match checked {
                Err(refused) => {
                    let text = refused.to_string();
                    refusal = Some(refused);
                    bail!("{text}")
                }
                Ok(authorization) => {
                    let uses = usage_entries(&authorization.decision.usage);
                    let at = now.unix_secs();
                    ctx.store
                        .update_commands(lock, at, |book| book.mark_signing(id, uses, at))?;
                    signed = true;
                    Ok(())
                }
            }
        };
        tokio::time::timeout(
            SUBMIT_TIMEOUT,
            exec.ledger
                .submit_directed(req.clone(), &expectation, &command, &mut bind),
        )
        .await
    };
    let at = rpc.checked_now().map_or(at, |now| now.unix_secs());
    let refusal_text = refusal.as_ref().map(ToString::to_string);
    let submitted = classify(result, signed, refusal);
    alert_for(ctx, &id, &submitted, refusal_text.as_deref());
    match submitted {
        Submitted::Executed(update_id) => {
            info!("agent control: command {id} executed: update {update_id}");
            let end = CommandEnd::Executed {
                update_id: update_id.clone(),
            };
            let delivered = settle(
                exec.answers(),
                lock,
                &state,
                &pending.id,
                pending.hash,
                end,
                false,
            )
            .await?;
            Ok(Handled::Executed {
                update_id,
                delivered,
            })
        }
        Submitted::Answered {
            refused,
            code,
            name,
            from_ledger,
        } => {
            warn!(
                "agent control: command {id} {} before its signature: {name} (code {code})",
                if refused { "refused" } else { "failed" }
            );
            let end = if refused {
                CommandEnd::Refused { code }
            } else {
                CommandEnd::Failed { code }
            };
            let delivered = settle(
                exec.answers(),
                lock,
                &state,
                &pending.id,
                pending.hash,
                end,
                from_ledger,
            )
            .await?;
            Ok(if refused {
                Handled::Refused {
                    code,
                    name,
                    delivered,
                }
            } else {
                Handled::Failed {
                    code,
                    name,
                    delivered,
                }
            })
        }
        Submitted::Again(why) => {
            ctx.store
                .update_commands(&lock, at, |book| book.release(&id, at))?;
            Ok(Handled::NotYet(why))
        }
        Submitted::Open { verdict, why } => {
            warn!(
                "agent control: command {id} was signed and its outcome is not known yet ({why}); it is reconciled, never signed again"
            );
            if let Some(verdict) = verdict {
                ctx.store
                    .update_commands(&lock, at, |book| book.set_verdict(&id, verdict, at))?;
            }
            drop(lock);
            reconcile(exec, &id).await
        }
    }
}

/// One listed command: settled, verified, then executed unless it waits.
pub async fn run_pending(
    exec: &mut Executor<'_>,
    item: &PendingCommand,
    mode: Mode,
) -> Result<Handled> {
    ensure!(
        mode != Mode::Confirm,
        "a confirmed command runs through `control execute`"
    );
    match admit(exec, item, mode).await? {
        Admission::Done(handled) => Ok(handled),
        Admission::Ready(ready) => dispatch(exec, *ready).await,
    }
}

/// The attempt of a verified command. Unsigned after a 4103 or a 4106, it runs once more
/// after the control log is synced or a new ledger token minted (C13).
async fn dispatch(exec: &mut Executor<'_>, ready: Ready) -> Result<Handled> {
    let again = ready.clone();
    let handled = dispatch_once(exec, ready).await?;
    let code = match &handled {
        Handled::NotYet(why) => refusal_code(why),
        _ => None,
    };
    let id = &again.pending.id;
    match code {
        Some(LOG_BEHIND) => {
            info!(
                "agent control: command {id}: the ledger-service lacks the newest control entry; syncing the log"
            );
            match tokio::time::timeout(SYNC_TIMEOUT, exec.rpc.sync_log(exec.ctx)).await {
                Ok(Ok(())) => dispatch_once(exec, again).await,
                Ok(Err(e)) => {
                    warn!(
                        "agent control: command {id}: the control log could not be synced: {e:#}"
                    );
                    Ok(handled)
                }
                Err(_) => {
                    warn!(
                        "agent control: command {id}: the control log was not synced within {SYNC_TIMEOUT:?}"
                    );
                    Ok(handled)
                }
            }
        }
        Some(TOKEN_REFUSED) => {
            info!(
                "agent control: command {id}: the ledger-service refused the token's control claim; minting a new one"
            );
            exec.ledger.remint();
            dispatch_once(exec, again).await
        }
        _ => Ok(handled),
    }
}

/// One attempt: a swap through its RFQ rounds, a transfer directly.
async fn dispatch_once(exec: &mut Executor<'_>, ready: Ready) -> Result<Handled> {
    if ready.is_swap() {
        swap::attempt(exec, ready).await
    } else {
        attempt(exec, ready).await
    }
}

/// The open records of the local book, beyond what the rpc lists: executing ones are
/// reconciled and undelivered receipts posted again.
pub async fn settle_open(
    exec: &mut Executor<'_>,
    skip: &BTreeSet<String>,
) -> Result<Vec<(String, Handled)>> {
    let book: CommandBook = {
        let lock = exec.ctx.store.lock().await?;
        exec.ctx.store.commands(&lock)?
    };
    let open: Vec<CommandRecord> = book
        .executing()
        .chain(book.unposted())
        .filter(|r| !skip.contains(&r.command_id))
        .take(MAX_OPEN_PER_ROUND)
        .cloned()
        .collect();
    let mut done = Vec::new();
    for record in open {
        let handled = if record.status == CommandStatus::Executing {
            reconcile(exec, &record.command_id).await?
        } else {
            redeliver(exec.answers(), &record).await
        };
        done.push((record.command_id, handled));
    }
    Ok(done)
}

/// The account of `handled` for the terminal.
fn write_handled(out: &mut dyn Write, id: &Uuid7, handled: &Handled) {
    let receipt = |delivered: bool| {
        if delivered {
            "The agent's signed receipt was sent to the rpc."
        } else {
            "The agent's signed receipt is kept and posted again by the daemon or the next `control execute`."
        }
    };
    let _ = match handled {
        Handled::Executed {
            update_id,
            delivered,
        } => writeln!(out, "Executed: update {update_id}. {}", receipt(*delivered)),
        Handled::Refused {
            code,
            name,
            delivered,
        } => writeln!(
            out,
            "Refused: {name} (code {code}). {}",
            receipt(*delivered)
        ),
        Handled::Failed {
            code,
            name,
            delivered,
        } => {
            writeln!(
                out,
                "Failed: {name} (code {code}); nothing was transferred. {}",
                receipt(*delivered)
            )
        }
        Handled::Unsettled(why) => writeln!(
            out,
            "Signed; the outcome is not known yet ({why}). It is never signed again: run \
             `cloud-agent control execute --code {CODE_PREFIX}{id}` later, or let the daemon settle it."
        ),
        Handled::Waiting => writeln!(
            out,
            "Waiting for `cloud-agent control execute --code {CODE_PREFIX}{id}`."
        ),
        Handled::NotYet(why) => writeln!(out, "Nothing was signed: {why}. Retry."),
        Handled::Settled { status, delivered } => writeln!(
            out,
            "Command {CODE_PREFIX}{id} was settled before ({}).{}",
            status_text(*status),
            if *delivered {
                " Its receipt was sent again."
            } else {
                ""
            }
        ),
    };
}

fn status_text(status: CommandStatus) -> &'static str {
    match status {
        CommandStatus::Received => "received",
        CommandStatus::Executing => "executing",
        CommandStatus::Executed => "executed",
        CommandStatus::Refused => "refused",
        CommandStatus::Failed => "failed",
    }
}

/// The exit of `control execute`: anything but an execution is an error.
pub fn exit_status(handled: &Handled) -> Result<()> {
    match handled {
        Handled::Executed { .. }
        | Handled::Settled {
            status: CommandStatus::Executed,
            ..
        } => Ok(()),
        Handled::Refused { code, name, .. } => {
            bail!("the command was refused: {name} (code {code})")
        }
        Handled::Failed { code, name, .. } => bail!("the command failed: {name} (code {code})"),
        Handled::Unsettled(_) => bail!("the command's outcome is not known yet"),
        Handled::NotYet(why) => bail!("nothing was signed: {why}"),
        Handled::Waiting => bail!("the command was not run"),
        Handled::Settled { status, .. } => bail!("the command was {} before", status_text(*status)),
    }
}

/// A command the rpc does not list: only what the local book knows is settled.
async fn settle_known(exec: &mut Executor<'_>, id: &str) -> Result<Handled> {
    let record = {
        let lock = exec.ctx.store.lock().await?;
        record_of(&exec.ctx.store, &lock, id)?
    };
    match record {
        None => bail!("command {CODE_PREFIX}{id} is not pending for this agent"),
        Some(record) if record.status == CommandStatus::Executing => reconcile(exec, id).await,
        Some(record) if record.status.is_final() => Ok(redeliver(exec.answers(), &record).await),
        Some(_) => bail!(
            "command {CODE_PREFIX}{id} is no longer pending at the rpc; nothing was signed for it"
        ),
    }
}

/// `control execute --code CMD-<id>`: the command is verified, shown and confirmed by the
/// typed `transfer <amount> <asset>`, then executed once with a signed receipt.
pub async fn run_execute(
    ctx: &ControlContext,
    rpc: &dyn CommandRpc,
    ledger: &mut dyn DirectedLedger,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    code: &str,
) -> Result<Handled> {
    let id = parse_code(code)?;
    let listed = match tokio::time::timeout(RPC_TIMEOUT, rpc.pending_commands()).await {
        Ok(Ok(listed)) => listed,
        Ok(Err(e)) => bail!("listing the pending commands failed: {e}"),
        Err(_) => bail!("the pending commands were not listed within {RPC_TIMEOUT:?}"),
    };
    let mut exec = Executor { ctx, rpc, ledger };
    let handled = match listed.iter().find(|c| c.command_id == id.as_str()) {
        None => settle_known(&mut exec, id.as_str()).await?,
        Some(item) => match admit(&mut exec, item, Mode::Confirm).await? {
            Admission::Done(handled) => handled,
            Admission::Ready(ready) => {
                ready.write_summary(out);
                confirm_typed(prompter, out, &ready.phrase(), Yes::Refused).await?;
                dispatch(&mut exec, *ready).await?
            }
        },
    };
    write_handled(out, &id, &handled);
    Ok(handled)
}

/// One pending command as `control commands` lists it.
fn describe(item: &PendingCommand, book: &CommandBook) -> String {
    let local = book
        .get(&item.command_id)
        .map_or("new", |r| status_text(r.status));
    let signer = ProofEnvelope::decode(&item.proof)
        .map_or_else(|_| "?".to_string(), |e| e.controller_id().fingerprint());
    match Command::decode(&item.command) {
        Ok(command) => {
            let what = match &command.op {
                CommandOp::TransferCc {
                    receiver, amount, ..
                } => format!("transfer_cc {amount} CC to {receiver}"),
                CommandOp::TransferToken {
                    receiver,
                    instrument,
                    amount,
                    ..
                } => {
                    format!(
                        "transfer_token {amount} {} ({}) to {receiver}",
                        instrument.id, instrument.admin
                    )
                }
                CommandOp::UsdcxBurn {
                    amount,
                    destination,
                } => format!("usdcx_burn {amount} to {destination}"),
                CommandOp::Swap {
                    sell,
                    sell_amount,
                    buy,
                    min_receive,
                    ..
                } => {
                    format!(
                        "swap {sell_amount} {} for at least {min_receive} {}",
                        sell.id, buy.id
                    )
                }
            };
            format!(
                "{CODE_PREFIX}{}  {what}  expires {}  signed by {signer}  [{local}]",
                command.command_id, command.expires_at
            )
        }
        Err(e) => format!(
            "{CODE_PREFIX}{}  (does not decode: {})  [{local}]",
            item.command_id,
            e.code_name()
        ),
    }
}

/// `control commands`: the direct commands waiting for this agent, and the local ones still open.
pub async fn run_list(
    ctx: &ControlContext,
    rpc: &dyn CommandRpc,
    out: &mut dyn Write,
) -> Result<()> {
    let mut listed = match tokio::time::timeout(RPC_TIMEOUT, rpc.pending_commands()).await {
        Ok(Ok(listed)) => listed,
        Ok(Err(e)) => bail!("listing the pending commands failed: {e}"),
        Err(_) => bail!("the pending commands were not listed within {RPC_TIMEOUT:?}"),
    };
    listed.sort_by(|a, b| {
        let key = |c: &PendingCommand| {
            c.submitted_at
                .as_ref()
                .map_or((i64::MAX, i32::MAX), |t| (t.seconds, t.nanos))
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| a.command_id.cmp(&b.command_id))
    });
    let book = {
        let lock = ctx.store.lock().await?;
        ctx.store.commands(&lock)?
    };
    if listed.is_empty() {
        let _ = writeln!(out, "No direct command is pending for this agent.");
    } else {
        let _ = writeln!(out, "Pending direct commands, oldest first:");
        for item in &listed {
            let _ = writeln!(out, "  {}", describe(item, &book));
        }
        let _ = writeln!(
            out,
            "Run one with `cloud-agent control execute --code {CODE_PREFIX}<command id>`."
        );
    }
    let listed_ids: BTreeSet<&str> = listed.iter().map(|c| c.command_id.as_str()).collect();
    for record in book
        .executing()
        .chain(book.unposted())
        .filter(|r| !listed_ids.contains(r.command_id.as_str()))
    {
        let _ = writeln!(
            out,
            "  {CODE_PREFIX}{}  {}{}",
            record.command_id,
            status_text(record.status),
            if record.status == CommandStatus::Executing {
                ": its outcome is not settled yet"
            } else {
                ": its receipt is not delivered yet"
            }
        );
    }
    Ok(())
}

/// Opens the ledger a daemon round uses.
pub type LedgerFactory = Arc<dyn Fn(&BaseConfig) -> Box<dyn DirectedLedger> + Send + Sync>;

/// The daemon's executor: with `auto_execute_commands` it runs each valid command, oldest
/// first; otherwise it answers only those that can never run. Open records are settled.
pub struct DaemonExecutor {
    ledgers: LedgerFactory,
    /// Where receipts go instead of the poller's rpc (tests).
    receipts: Option<Arc<dyn CommandRpc>>,
    announced: Mutex<BTreeSet<String>>,
}

impl Default for DaemonExecutor {
    fn default() -> Self {
        Self {
            ledgers: Arc::new(|config: &BaseConfig| {
                Box::new(LazyLedger::new(config.clone())) as Box<dyn DirectedLedger>
            }),
            receipts: None,
            announced: Mutex::new(BTreeSet::new()),
        }
    }
}

impl DaemonExecutor {
    /// An executor over other ledgers and receipts, for drills.
    pub fn with(ledgers: LedgerFactory, receipts: Option<Arc<dyn CommandRpc>>) -> Self {
        Self {
            ledgers,
            receipts,
            announced: Mutex::new(BTreeSet::new()),
        }
    }

    fn first(&self, id: &str) -> bool {
        let mut announced = agent_logic::sync::lock(&self.announced);
        if announced.len() >= 1024 {
            announced.clear();
        }
        announced.insert(id.to_string())
    }

    fn log(&self, id: &str, handled: &Handled) {
        match handled {
            Handled::Waiting => {
                if self.first(id) {
                    info!(
                        "agent control: command {id} waits for `cloud-agent control execute --code {CODE_PREFIX}{id}` \
                         ([control] auto_execute_commands = false)"
                    );
                }
            }
            Handled::NotYet(why) => debug!("agent control: command {id} not run yet: {why}"),
            Handled::Settled { status, delivered } => {
                debug!(
                    "agent control: command {id} was settled before ({}); receipt delivered: {delivered}",
                    status_text(*status)
                );
            }
            Handled::Unsettled(why) => {
                info!("agent control: command {id} is not settled yet: {why}")
            }
            other => info!("agent control: command {id}: {other:?}"),
        }
    }
}

#[async_trait::async_trait]
impl CommandExecutor for DaemonExecutor {
    async fn execute(
        &self,
        ctx: &ControlContext,
        rpc: &ControlRpc,
        command: &PendingCommand,
    ) -> Result<()> {
        let rpc: &dyn CommandRpc = match &self.receipts {
            Some(receipts) => receipts.as_ref(),
            None => rpc,
        };
        let mode = if ctx.config.control.auto_execute_commands {
            Mode::Automatic
        } else {
            Mode::Waiting
        };
        let mut ledger = (self.ledgers)(&ctx.config);
        let mut exec = Executor {
            ctx,
            rpc,
            ledger: ledger.as_mut(),
        };
        let handled = run_pending(&mut exec, command, mode).await?;
        self.log(&command.command_id, &handled);
        Ok(())
    }

    async fn settle_open(
        &self,
        ctx: &ControlContext,
        rpc: &ControlRpc,
        listed: &[PendingCommand],
    ) -> Result<()> {
        let rpc: &dyn CommandRpc = match &self.receipts {
            Some(receipts) => receipts.as_ref(),
            None => rpc,
        };
        let skip: BTreeSet<String> = listed.iter().map(|c| c.command_id.clone()).collect();
        let mut ledger = (self.ledgers)(&ctx.config);
        let mut exec = Executor {
            ctx,
            rpc,
            ledger: ledger.as_mut(),
        };
        for (id, handled) in settle_open(&mut exec, &skip).await? {
            self.log(&id, &handled);
        }
        Ok(())
    }
}

#[path = "swap.rs"]
pub(crate) mod swap;

#[cfg(test)]
pub(crate) mod tests;
