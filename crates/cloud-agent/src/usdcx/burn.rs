//! Withdrawals: burn USDCx on Canton for a release of USDC on Ethereum.
//!
//! A burn is never started while an earlier withdrawal of the party may still
//! commit, and a withdrawal is retried only under its own request id: every
//! signature is recorded in the burn log before it is made, and an unclear
//! outcome is settled from the ledger (the attestation by request id, the
//! transaction by command id, or an input holding still unspent once no burn
//! could commit any more) before the command ends.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::str::FromStr;
use std::time::Duration;

use agent_logic::clock;
use anyhow::{Context, Result, anyhow, bail, ensure};
use orderbook_proto::ledger::{
    PrepareTransactionRequest, TransactionOperation, UsdcxBurnParams, UsdcxReclaimHoldingParams,
    prepare_transaction_request::Params,
};
use rust_decimal::Decimal;
use tx_verifier::OperationExpectation;
use tx_verifier::usdcx::{burn_command_id, reclaim_command_id};

use super::acs::{self, WithdrawalSeen};
use super::state::{BURNS_FILE, BurnEntry, BurnLog, BurnStatus, LogLock, Signature};
use super::{Ctx, circle, ethaddr, in_doubt, pending};
use crate::ledger_client::{PreSignContext, SubmitOptions};

/// After this long without the burn on the ledger, no signature for it can
/// still commit (the server applies a shorter window of its own).
const BURN_WINDOW_MINUTES: i64 = 15;
const SETTLE_POLL: Duration = Duration::from_secs(15);
const OUTCOME_POLL: Duration = Duration::from_secs(20);
const FEE_QUOTE_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) struct BurnArgs {
    pub amount: String,
    pub to: String,
    pub request_id: Option<String>,
    pub fee_on_top: bool,
    pub timeout: Duration,
}

/// A positive USDC amount with at most 6 decimals, normalized.
pub(crate) fn parse_amount(raw: &str) -> Result<Decimal> {
    let d = Decimal::from_str(raw.trim())
        .map_err(|e| anyhow!("amount {raw:?} is not a decimal: {e}"))?
        .normalize();
    ensure!(d > Decimal::ZERO, "amount must be positive");
    ensure!(
        d.scale() <= 6,
        "amount {raw} has more than 6 decimals (USDC has 6)"
    );
    Ok(d)
}

/// The destination as `0x` + 40 lowercase hex (EIP-55 checked when mixed case).
fn parse_destination(raw: &str) -> Result<String> {
    let address = ethaddr::parse_eth_address(raw.trim())?;
    let lower = ethaddr::lower(address);
    ensure!(
        lower != "0x0000000000000000000000000000000000000000",
        "the destination must not be the zero address"
    );
    Ok(lower)
}

/// A request id given on the command line: a UUID v4, lowercase.
fn parse_request_id(raw: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(raw.trim()).map_err(|e| anyhow!("--request-id {raw:?}: {e}"))?;
    ensure!(
        id.get_version_num() == 4,
        "--request-id {raw} is not a UUID v4"
    );
    Ok(id.hyphenated().to_string())
}

/// The `key=value` tokens that follow `marker` in an error text.
fn marker_fields(text: &str, marker: &str) -> Option<Vec<(String, String)>> {
    let (_, rest) = text.split_once(marker)?;
    let fields = rest
        .split_whitespace()
        .map_while(|token| {
            let (key, value) = token.split_once('=')?;
            let value = value
                .trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
            Some((key.to_string(), value.to_string()))
        })
        .collect();
    Some(fields)
}

fn field(fields: &[(String, String)], key: &str) -> Option<String> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty() && v != "-")
}

/// What the ledger says about one withdrawal.
pub(crate) enum Settled {
    Committed(WithdrawalSeen),
    /// None of its signatures committed or ever can.
    NotCommitted(String),
    /// It may still commit.
    Unsettled(String),
}

/// Settles a signed withdrawal from the ledger.
pub(crate) async fn settle(ctx: &mut Ctx, entry: &BurnEntry) -> Result<Settled> {
    let request_id = entry.request_id.as_str();
    let mut reader = ctx.reader();
    if let Some(w) = reader
        .withdraw_attestations()
        .await?
        .into_iter()
        .find(|w| w.request_id == request_id)
    {
        return Ok(Settled::Committed(WithdrawalSeen {
            attestation_cid: Some(w.cid),
            ..Default::default()
        }));
    }
    if let Some(w) = reader
        .accepted_withdrawals()
        .await?
        .into_iter()
        .find(|w| w.request_id == request_id)
    {
        return Ok(Settled::Committed(WithdrawalSeen {
            accepted: Some((w.cid, w.detail)),
            ..Default::default()
        }));
    }
    if let Some(w) = reader
        .revoked_withdrawals()
        .await?
        .into_iter()
        .find(|w| w.request_id == request_id)
    {
        return Ok(Settled::Committed(WithdrawalSeen {
            revoked: Some((w.cid, w.detail)),
            ..Default::default()
        }));
    }
    // DA may already have archived its outcome: the transactions since the
    // first signature.
    let scan = acs::scan(
        &mut ctx.client,
        entry.ledger_offset,
        &acs::withdraw_filters(),
    )
    .await;
    let complete = match &scan {
        Ok(s) => {
            let seen = acs::find_withdrawal(
                &s.transactions,
                ctx.net,
                &ctx.party,
                request_id,
                &entry.command_ids,
            );
            if seen.committed() {
                return Ok(Settled::Committed(seen));
            }
            s.complete
        }
        Err(_) => false,
    };
    let window = chrono::Duration::minutes(BURN_WINDOW_MINUTES);
    let since = clock::now_utc().signed_duration_since(entry.last_signed_at);
    if since <= window {
        return Ok(Settled::Unsettled(format!(
            "signed {}s ago and not on the ledger yet",
            since.num_seconds()
        )));
    }
    if complete {
        return Ok(Settled::NotCommitted(format!(
            "no transaction with command id {} since offset {}",
            entry.command_ids.join(", "),
            entry.ledger_offset
        )));
    }
    // Every signature spent the shared holdings: one still active means none committed.
    let holdings = ctx.reader().holdings().await?;
    if let Some(h) = entry
        .shared_holdings
        .iter()
        .find(|h| holdings.iter().any(|a| &a.cid == *h))
    {
        return Ok(Settled::NotCommitted(format!(
            "its input holding {h} is still unspent"
        )));
    }
    Ok(Settled::Unsettled(match scan {
        Err(e) => format!("the ledger could not be scanned: {e:#}"),
        Ok(_) => "the ledger scan was incomplete".to_string(),
    }))
}

/// Records what the ledger said about a withdrawal in the log.
fn apply(log: &mut BurnLog, request_id: &str, settled: &Settled) {
    let Some(entry) = log.get_mut(request_id) else {
        return;
    };
    match settled {
        Settled::Committed(seen) => {
            if !entry.status.is_committed() {
                entry.status = BurnStatus::Committed;
            }
            if seen.burn_update.is_some() {
                entry.update_id.clone_from(&seen.burn_update);
            }
            if seen.attestation_cid.is_some() {
                entry.attestation_cid.clone_from(&seen.attestation_cid);
            }
            if let Some((_, trace)) = &seen.accepted {
                entry.status = BurnStatus::Accepted;
                entry.detail.clone_from(trace);
            }
            if let Some((cid, reason)) = &seen.revoked {
                if entry.status != BurnStatus::Reclaimed {
                    entry.status = BurnStatus::Revoked;
                }
                entry.detail.clone_from(reason);
                entry.revoked_cid = Some(cid.clone());
            }
        }
        Settled::NotCommitted(why) => {
            entry.status = BurnStatus::Failed;
            entry.detail = Some(why.clone());
        }
        Settled::Unsettled(_) => {}
    }
}

/// How a submission ended, as far as the command can tell.
enum Submitted {
    Committed {
        update_id: Option<String>,
        attestation_cid: Option<String>,
    },
    /// Definitively rejected after signing: this signature did not commit.
    Rejected(String),
    /// It may have committed.
    Unknown(String),
    /// Nothing was signed.
    NotSigned(anyhow::Error),
}

fn classify(
    result: Result<orderbook_proto::ledger::ExecuteTransactionResponse>,
    signed: bool,
) -> Submitted {
    match result {
        Ok(r) if r.success => Submitted::Committed {
            update_id: Some(r.update_id).filter(|u| !u.is_empty()),
            attestation_cid: r.contract_id.filter(|c| !c.is_empty()),
        },
        Ok(r) => {
            let msg = r
                .error_message
                .unwrap_or_else(|| "not executed".to_string());
            if signed {
                Submitted::Unknown(msg)
            } else {
                Submitted::NotSigned(anyhow!(msg))
            }
        }
        Err(e) => {
            let text = format!("{e:#}");
            // The agent's own refusal before signing: no marker its text quotes counts.
            if crate::ledger_client::is_presign_refusal(&e) {
                return if signed { Submitted::Rejected(text) } else { Submitted::NotSigned(e) };
            }
            if let Some(fields) = marker_fields(&text, "USDCX_BURN_COMMITTED") {
                return Submitted::Committed {
                    update_id: field(&fields, "update_id"),
                    attestation_cid: field(&fields, "attestation_cid"),
                };
            }
            if !signed {
                return Submitted::NotSigned(e);
            }
            // An earlier execute that may still commit, or the server's own
            // unknown outcome, decides first.
            if crate::ledger_client::is_ambiguous_execute_error(&e) || text.contains("USDCX_OUTCOME_UNKNOWN") {
                return Submitted::Unknown(text);
            }
            // The server refused before submitting this signature (its reason
            // may quote another request's open burn).
            if text.contains("USDCX_NOT_SUBMITTED") {
                return Submitted::Rejected(text);
            }
            if !in_doubt(&e) && text.contains("Transaction failed:") {
                Submitted::Rejected(text)
            } else {
                Submitted::Unknown(text)
            }
        }
    }
}

/// `usdcx burn`.
pub(crate) async fn burn(ctx: &mut Ctx, args: BurnArgs) -> Result<()> {
    let amount = parse_amount(&args.amount)?;
    let to = parse_destination(&args.to)?;
    let request_id = match args.request_id.as_deref() {
        Some(raw) => parse_request_id(raw)?,
        None => clock::uuid_v4()?.to_string(),
    };
    agent_logic::out!(
        "=== USDCx withdrawal: Canton -> {} ({}) ===",
        ctx.eth.eth_chain_name,
        ctx.net.name
    );
    agent_logic::out!("  Request id:   {request_id}");
    agent_logic::out!(
        "                (if this run is interrupted, continue with `usdcx burn --request-id {request_id} …` or `usdcx track`)"
    );

    let lock = LogLock::acquire(&ctx.dir, BURNS_FILE).await?;
    let mut log = BurnLog::load(&ctx.dir)?;

    // Every other withdrawal must be settled before a new burn.
    let others: Vec<BurnEntry> = log
        .unsettled()
        .filter(|b| b.request_id != request_id)
        .cloned()
        .collect();
    for other in others {
        let settled = settle(ctx, &other).await?;
        apply(&mut log, &other.request_id, &settled);
        log.save(&ctx.dir)?;
        if let Settled::Unsettled(why) = settled {
            return Err(pending(format!(
                "withdrawal {} may still commit ({why}). Finish it with `usdcx burn --request-id {} \
                 --amount {} --to {}` or `usdcx track`; do not start another withdrawal",
                other.request_id, other.request_id, other.amount, other.destination
            )));
        }
    }

    // A known request id is continued, never repeated with other terms.
    let mut inputs: Vec<String> = Vec::new();
    if let Some(entry) = log.get(&request_id).cloned() {
        if entry.status.is_committed() {
            agent_logic::out!("✓ Withdrawal {request_id} has already burned");
            drop(lock);
            return wait_outcome(ctx, &request_id, args.timeout).await;
        }
        if entry.status == BurnStatus::Signed {
            let settled = settle(ctx, &entry).await?;
            apply(&mut log, &request_id, &settled);
            log.save(&ctx.dir)?;
            match settled {
                Settled::Committed(_) => {
                    agent_logic::out!("✓ The burn of withdrawal {request_id} committed");
                    drop(lock);
                    return wait_outcome(ctx, &request_id, args.timeout).await;
                }
                Settled::Unsettled(why) => {
                    return Err(pending(format!(
                        "the earlier burn of withdrawal {request_id} may still commit ({why}); \
                         run this command again later"
                    )));
                }
                Settled::NotCommitted(why) => {
                    agent_logic::out!("  The earlier signature did not commit ({why}); retrying");
                }
            }
        }
        inputs.clone_from(&entry.shared_holdings);
    }

    let eth = ctx.eth;

    let agreement = ctx.reader().require_agreement(eth).await?;

    // Circle's fee is taken from the burned amount on release. A retry keeps
    // the burn amount and destination of its first signature.
    let quote = fee_quote(ctx, &to, amount, args.fee_on_top).await;
    let recorded = log
        .get(&request_id)
        .map(|e| parse_amount(&e.amount).map(|a| (a, e.destination.clone())))
        .transpose()?;
    let (burn_amount, fee) = match recorded {
        Some((recorded, destination)) => {
            ensure!(
                destination == to,
                "withdrawal {request_id} was signed for destination {destination}; a request id is \
                 never reused for other terms"
            );
            ensure!(
                recorded == amount || (args.fee_on_top && amount < recorded),
                "withdrawal {request_id} was signed for {recorded} USDCx; a request id is never \
                 reused for other terms"
            );
            agent_logic::out!("  Continuing withdrawal {request_id} with its first terms");
            (recorded, quote.ok())
        }
        None => match (&quote, args.fee_on_top) {
            (Ok(fee), true) => (
                amount
                    .checked_add(*fee)
                    .ok_or_else(|| anyhow!("amount overflow"))?,
                Some(*fee),
            ),
            (Ok(fee), false) => (amount, Some(*fee)),
            (Err(e), true) => {
                bail!("--fee-on-top needs Circle's fee quote, which is unavailable: {e:#}")
            }
            (Err(_), false) => (amount, None),
        },
    };
    ensure!(
        burn_amount >= agreement.min_burn_value,
        "burn amount {burn_amount} is below the bridge minimum {} USDCx",
        agreement.min_burn_value
    );
    if let Some(fee) = fee {
        ensure!(
            burn_amount > fee,
            "burn amount {burn_amount} does not cover Circle's fee (~{fee} USDC)"
        );
    }
    let holdings = ctx.reader().holdings().await?;
    let unlocked = holdings
        .iter()
        .filter(|h| !h.locked)
        .try_fold(Decimal::ZERO, |sum, h| sum.checked_add(h.amount))
        .ok_or_else(|| anyhow!("balance overflow"))?;
    ensure!(
        unlocked >= burn_amount,
        "{} holds {unlocked} unlocked USDCx; the burn needs {burn_amount}",
        ctx.party
    );

    let burn_text = burn_amount.normalize().to_string();
    agent_logic::out!("  From:         {}", ctx.party);
    agent_logic::out!("  Burn:         {burn_text} USDCx (of {unlocked} unlocked)");
    agent_logic::out!("  Destination:  {to} on {}", ctx.eth.eth_chain_name);
    match fee {
        Some(fee) => {
            let receive = burn_amount.checked_sub(fee).unwrap_or(Decimal::ZERO);
            agent_logic::out!("  Circle fee:   {fee} USDC (quote)");
            agent_logic::out!("  You receive:  ~{receive} USDC, released automatically by Circle");
        }
        None => agent_logic::out!(
            "  Circle fee:   quote unavailable; Circle deducts its fee from the release"
        ),
    }
    ctx.confirm(
        true,
        "Burn USDCx (irreversible)",
        &format!("{burn_text} USDCx to {to}, request {request_id}"),
    )
    .await?;

    // Submit: one attempt; every signature is recorded before it is made.
    let ledger_offset = ctx.client.get_ledger_end().await?;
    let command_id = burn_command_id(&request_id);
    let expectation = OperationExpectation::UsdcxBurn {
        party: ctx.party.clone(),
        network: ctx.net.name.to_string(),
        amount: burn_text.clone(),
        destination_recipient: to.clone(),
        request_id: request_id.clone(),
        command_id: command_id.clone(),
    };
    let request = PrepareTransactionRequest {
        operation: TransactionOperation::UsdcxBurn as i32,
        params: Some(Params::UsdcxBurn(UsdcxBurnParams {
            amount: burn_text.clone(),
            destination_recipient: to.clone(),
            request_id: request_id.clone(),
            input_holding_cids: inputs,
            max_input_holdings: None,
        })),
        request_signature: None,
        controller_command: None,
    };
    let dir = ctx.dir.clone();
    let mut signed = false;
    let result = {
        let mut record = |pc: &PreSignContext<'_>| -> Result<()> {
            let mut next = BurnLog::load(&dir)?;
            next.record_signature(&Signature {
                request_id: &request_id,
                amount: &burn_text,
                destination: &to,
                holdings: &pc.verification.burn_holding_cids,
                command_id: pc.command_id,
                ledger_offset,
                at: clock::now_utc(),
            })?;
            next.save(&dir)?;
            signed = true;
            Ok(())
        };
        let opts = SubmitOptions {
            max_retries: 1,
            pre_sign: Some(&mut record),
        };
        ctx.client
            .submit_transaction_guarded(
                request,
                &expectation,
                ctx.verbose,
                ctx.dry_run,
                false,
                opts,
            )
            .await
    };
    if ctx.dry_run {
        return result.map(|_| agent_logic::out!("(dry run — nothing was signed or submitted)"));
    }
    let mut log = BurnLog::load(&ctx.dir)?;
    match classify(result, signed) {
        Submitted::NotSigned(e) => {
            if format!("{e:#}").contains("USDCX_BURN_PENDING") {
                agent_logic::out!(
                    "  An earlier burn of this party may still commit (the request named below): \
                     retry later — do not start another withdrawal meanwhile"
                );
            }
            return Err(e.context(format!("nothing was signed for withdrawal {request_id}")));
        }
        Submitted::Committed {
            update_id,
            attestation_cid,
        } => {
            let seen = WithdrawalSeen {
                burn_update: update_id.clone(),
                attestation_cid,
                ..Default::default()
            };
            if log.get(&request_id).is_some() {
                apply(&mut log, &request_id, &Settled::Committed(seen));
                log.save(&ctx.dir)?;
            }
            agent_logic::out!(
                "✓ Burned {burn_text} USDCx (update {})",
                update_id.as_deref().unwrap_or("-")
            );
        }
        Submitted::Rejected(why) => {
            apply(&mut log, &request_id, &Settled::NotCommitted(why.clone()));
            log.save(&ctx.dir)?;
            bail!("the burn was rejected; nothing was burned: {why}");
        }
        Submitted::Unknown(why) => {
            agent_logic::out!("⚠ The burn's outcome is unknown ({why}); checking the ledger");
            settle_until_known(ctx, &request_id).await?;
        }
    }
    drop(lock);
    wait_outcome(ctx, &request_id, args.timeout).await
}

/// Settles one withdrawal, polling until its outcome is known or no
/// signature could still commit. Ends with a pending error when neither.
async fn settle_until_known(ctx: &mut Ctx, request_id: &str) -> Result<()> {
    let window = chrono::Duration::minutes(BURN_WINDOW_MINUTES.saturating_add(1));
    loop {
        let mut log = BurnLog::load(&ctx.dir)?;
        let entry = log
            .get(request_id)
            .cloned()
            .ok_or_else(|| anyhow!("withdrawal {request_id} is not in the burn log"))?;
        let settled = settle(ctx, &entry).await?;
        apply(&mut log, request_id, &settled);
        log.save(&ctx.dir)?;
        match settled {
            Settled::Committed(_) => {
                agent_logic::out!("✓ The burn of withdrawal {request_id} committed");
                return Ok(());
            }
            Settled::NotCommitted(why) => bail!(
                "the burn of withdrawal {request_id} did not commit ({why}); nothing was burned. \
                 Retry with `usdcx burn --request-id {request_id} --amount {} --to {}`",
                entry.amount,
                entry.destination
            ),
            Settled::Unsettled(why) => {
                if clock::now_utc().signed_duration_since(entry.last_signed_at) > window {
                    return Err(pending(format!(
                        "the outcome of withdrawal {request_id} is still unknown ({why}); \
                         continue with `usdcx track` — do not start another withdrawal"
                    )));
                }
                agent_logic::out!("  … {why}");
                tokio::time::sleep(SETTLE_POLL).await;
            }
        }
    }
}

/// Waits for DA's outcome of a burned withdrawal.
async fn wait_outcome(ctx: &mut Ctx, request_id: &str, timeout: Duration) -> Result<()> {
    let deadline = clock::deadline_after(timeout);
    loop {
        let entry = BurnLog::load(&ctx.dir)?.get(request_id).cloned();
        let seen = outcome(ctx, request_id, entry.as_ref()).await?;
        if seen.accepted.is_some() || seen.revoked.is_some() {
            let _lock = LogLock::acquire(&ctx.dir, BURNS_FILE).await?;
            let mut log = BurnLog::load(&ctx.dir)?;
            apply(&mut log, request_id, &Settled::Committed(seen.clone()));
            log.save(&ctx.dir)?;
        }
        if let Some((_, trace)) = &seen.accepted {
            let destination = entry
                .as_ref()
                .map(|e| e.destination.as_str())
                .unwrap_or("-");
            agent_logic::out!(
                "✓ DA accepted withdrawal {request_id} (trace {}): Circle releases the USDC to {destination}",
                trace.as_deref().unwrap_or("-")
            );
            agent_logic::out!(
                "  Watch it arrive: {}{destination}",
                ctx.eth.explorer_tx.replace("/tx/", "/address/")
            );
            return Ok(());
        }
        if let Some((cid, reason)) = &seen.revoked {
            bail!(
                "DA revoked withdrawal {request_id} ({}): take the USDCx back with `usdcx reclaim \
                 --revoked-cid {cid}`",
                reason.as_deref().unwrap_or("no reason given")
            );
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(pending(format!(
                "withdrawal {request_id} burned; DA has not released it yet — continue with \
                 `usdcx track --wait`"
            )));
        }
        tokio::time::sleep(OUTCOME_POLL).await;
    }
}

/// DA's outcome of a withdrawal: from the ACS, else (DA may archive it) from
/// the transactions since its first signature.
async fn outcome(
    ctx: &mut Ctx,
    request_id: &str,
    entry: Option<&BurnEntry>,
) -> Result<WithdrawalSeen> {
    let mut reader = ctx.reader();
    if let Some(w) = reader
        .accepted_withdrawals()
        .await?
        .into_iter()
        .find(|w| w.request_id == request_id)
    {
        return Ok(WithdrawalSeen {
            accepted: Some((w.cid, w.detail)),
            ..Default::default()
        });
    }
    if let Some(w) = reader
        .revoked_withdrawals()
        .await?
        .into_iter()
        .find(|w| w.request_id == request_id)
    {
        return Ok(WithdrawalSeen {
            revoked: Some((w.cid, w.detail)),
            ..Default::default()
        });
    }
    if reader
        .withdraw_attestations()
        .await?
        .iter()
        .any(|w| w.request_id == request_id)
    {
        return Ok(WithdrawalSeen::default());
    }
    let Some(entry) = entry else {
        return Ok(WithdrawalSeen::default());
    };
    match acs::scan(
        &mut ctx.client,
        entry.ledger_offset,
        &acs::withdraw_filters(),
    )
    .await
    {
        Ok(s) => Ok(acs::find_withdrawal(
            &s.transactions,
            ctx.net,
            &ctx.party,
            request_id,
            &entry.command_ids,
        )),
        Err(e) => {
            tracing::warn!("withdrawal outcome scan failed: {e:#}");
            Ok(WithdrawalSeen::default())
        }
    }
}

/// Circle's fee for this withdrawal (the request DA's page sends).
async fn fee_quote(ctx: &Ctx, to: &str, amount: Decimal, fee_on_top: bool) -> Result<Decimal> {
    let http = reqwest::Client::builder()
        .timeout(FEE_QUOTE_TIMEOUT)
        .build()
        .context("HTTP client")?;
    circle::quote_withdrawal_fee(&http, ctx.eth, &ctx.party, to, amount, !fee_on_top).await
}

/// `usdcx track` for withdrawals: settles signed ones and follows burned ones.
pub(crate) async fn track(ctx: &mut Ctx, wait: bool, timeout: Duration) -> Result<()> {
    let open: Vec<BurnEntry> = BurnLog::load(&ctx.dir)?
        .burns
        .into_iter()
        .filter(|b| matches!(b.status, BurnStatus::Signed | BurnStatus::Committed))
        .collect();
    if open.is_empty() {
        agent_logic::out!("No open withdrawals");
        return Ok(());
    }
    let mut still_open = 0usize;
    let mut failed = 0usize;
    for entry in open {
        agent_logic::out!(
            "--- withdrawal {} ({} USDCx to {}, {:?})",
            entry.request_id,
            entry.amount,
            entry.destination,
            entry.status
        );
        if entry.status == BurnStatus::Signed {
            let _lock = LogLock::acquire(&ctx.dir, BURNS_FILE).await?;
            let mut log = BurnLog::load(&ctx.dir)?;
            let settled = settle(ctx, &entry).await?;
            apply(&mut log, &entry.request_id, &settled);
            log.save(&ctx.dir)?;
            match settled {
                Settled::Committed(_) => agent_logic::out!("✓ burned"),
                Settled::NotCommitted(why) => {
                    agent_logic::out!("✗ did not commit ({why}); nothing was burned");
                    failed = failed.saturating_add(1);
                    continue;
                }
                Settled::Unsettled(why) => {
                    agent_logic::out!("⚠ outcome still unknown ({why})");
                    still_open = still_open.saturating_add(1);
                    continue;
                }
            }
        }
        let budget = if wait { timeout } else { Duration::ZERO };
        match wait_outcome(ctx, &entry.request_id, budget).await {
            Ok(()) => {}
            Err(e) if super::is_pending(&e) => {
                agent_logic::out!("⚠ {e:#}");
                still_open = still_open.saturating_add(1);
            }
            Err(e) => {
                agent_logic::out!("✗ {e:#}");
                failed = failed.saturating_add(1);
            }
        }
    }
    if still_open > 0 {
        return Err(pending(format!("{still_open} withdrawal(s) still open")));
    }
    if failed > 0 {
        bail!("{failed} withdrawal(s) need attention");
    }
    Ok(())
}

/// `usdcx reclaim`: takes back the USDCx of a revoked withdrawal.
pub(crate) async fn reclaim(ctx: &mut Ctx, revoked_cid: &str) -> Result<()> {
    let eth = ctx.eth;
    ctx.reader().require_agreement(eth).await?;
    let revoked = ctx
        .reader()
        .revoked_withdrawals()
        .await?
        .into_iter()
        .find(|w| w.cid == revoked_cid)
        .ok_or_else(|| anyhow!("{revoked_cid} is not a revoked withdrawal of {}", ctx.party))?;
    agent_logic::out!(
        "Reclaiming {} USDCx of revoked withdrawal {} ({})",
        revoked.amount,
        revoked.request_id,
        revoked.detail.as_deref().unwrap_or("no reason given")
    );
    ctx.confirm(false, "Reclaim USDCx", revoked_cid).await?;
    let command_id = reclaim_command_id(revoked_cid);
    let expectation = OperationExpectation::UsdcxReclaimHolding {
        party: ctx.party.clone(),
        network: ctx.net.name.to_string(),
        revoked_withdraw_attestation_cid: revoked_cid.to_string(),
        command_id,
    };
    let request = PrepareTransactionRequest {
        operation: TransactionOperation::UsdcxReclaimHolding as i32,
        params: Some(Params::UsdcxReclaimHolding(UsdcxReclaimHoldingParams {
            revoked_withdraw_attestation_cid: revoked_cid.to_string(),
        })),
        request_signature: None,
        controller_command: None,
    };
    // The command id is keyed on the revoked attestation, which the reclaim
    // consumes: retries cannot commit twice.
    let opts = SubmitOptions {
        max_retries: 3,
        pre_sign: None,
    };
    let result = ctx
        .client
        .submit_transaction_guarded(request, &expectation, ctx.verbose, ctx.dry_run, false, opts)
        .await;
    if ctx.dry_run {
        return result.map(|_| agent_logic::out!("(dry run — nothing was signed or submitted)"));
    }
    match result {
        Ok(r) => {
            agent_logic::out!(
                "✓ Reclaimed {} USDCx (update {}, holding {})",
                revoked.amount,
                r.update_id,
                r.contract_id.as_deref().unwrap_or("-")
            );
            let _lock = LogLock::acquire(&ctx.dir, BURNS_FILE).await?;
            let mut log = BurnLog::load(&ctx.dir)?;
            if let Some(entry) = log.get_mut(&revoked.request_id) {
                entry.status = BurnStatus::Reclaimed;
                log.save(&ctx.dir)?;
            }
            Ok(())
        }
        Err(e) if in_doubt(&e) => Err(pending(format!(
            "the reclaim may have committed ({e:#}); run the same command again to finish it"
        ))),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_positive_with_six_decimals() {
        assert_eq!(parse_amount("10.500000").unwrap().to_string(), "10.5");
        assert!(parse_amount("0").is_err());
        assert!(parse_amount("-1").is_err());
        assert!(parse_amount("1.0000001").is_err());
        assert!(parse_amount("x").is_err());
    }

    #[test]
    fn request_ids_are_uuid_v4() {
        assert_eq!(
            parse_request_id("6F1C2C7E-4B8A-4C41-9A3E-2D5B7F0C9E11").unwrap(),
            "6f1c2c7e-4b8a-4c41-9a3e-2d5b7f0c9e11"
        );
        // A v7 is refused.
        assert!(parse_request_id("019a7c1e-6f3a-7c41-9a3e-2d5b7f0c9e11").is_err());
        assert!(parse_request_id("nope").is_err());
    }

    #[test]
    fn server_markers_are_read_from_error_text() {
        let text = "status: Internal, message: \"USDCX_BURN_COMMITTED request_id=r-1 update_id=1220ab attestation_cid=- status=burned\", details: []";
        let fields = marker_fields(text, "USDCX_BURN_COMMITTED").unwrap();
        assert_eq!(field(&fields, "request_id").as_deref(), Some("r-1"));
        assert_eq!(field(&fields, "update_id").as_deref(), Some("1220ab"));
        assert_eq!(field(&fields, "attestation_cid"), None);
        assert_eq!(field(&fields, "status").as_deref(), Some("burned"));
        assert!(marker_fields(text, "USDCX_BURN_PENDING").is_none());
    }

    #[test]
    fn submissions_are_classified_by_what_was_signed() {
        let err = |s: &str| -> Result<orderbook_proto::ledger::ExecuteTransactionResponse> {
            Err(anyhow!("{s}"))
        };
        // A committed marker wins, signed or not.
        assert!(matches!(
            classify(
                err("x: USDCX_BURN_COMMITTED request_id=r update_id=u"),
                false
            ),
            Submitted::Committed { .. }
        ));
        // Nothing signed: nothing submitted.
        assert!(matches!(
            classify(err("Transaction failed: X"), false),
            Submitted::NotSigned(_)
        ));
        // Signed, then a definite rejection.
        assert!(matches!(
            classify(err("Transaction failed: INSUFFICIENT_FUNDS"), true),
            Submitted::Rejected(_)
        ));
        assert!(matches!(
            classify(err("Transaction failed: USDCX_NOT_SUBMITTED: row"), true),
            Submitted::Rejected(_)
        ));
        // Not submitted, even when the reason quotes another request's open burn.
        assert!(matches!(
            classify(
                err(
                    "Transaction failed: USDCX_NOT_SUBMITTED: another burn of the party is open: USDCX_BURN_PENDING request_id=r command_id=c"
                ),
                true
            ),
            Submitted::Rejected(_)
        ));
        // Unless an earlier execute of this command may still commit.
        assert!(matches!(
            classify(
                err(
                    "EXECUTE_OUTCOME_UNKNOWN: earlier execute c may still commit: Transaction failed: USDCX_NOT_SUBMITTED: x"
                ),
                true
            ),
            Submitted::Unknown(_)
        ));
        // Signed, then anything in doubt.
        for doubt in [
            "EXECUTE_OUTCOME_UNKNOWN: earlier execute c may still commit: x",
            "Transaction failed: USDCX_OUTCOME_UNKNOWN command_id=c: timeout",
            "Transaction failed: DUPLICATE_COMMAND",
            "Command already submitted (DUPLICATE_COMMAND): x",
            "transport error",
        ] {
            assert!(
                matches!(classify(err(doubt), true), Submitted::Unknown(_)),
                "{doubt}"
            );
        }
    }

    #[test]
    fn a_refusal_before_signing_is_never_an_unknown_outcome_whatever_its_text() {
        use crate::control::intent::{ContentRefusal, PackageRefusal, SignatureRefusal};
        let markers = "Transaction failed: USDCX_BURN_COMMITTED request_id=r update_id=u EXECUTE_OUTCOME_UNKNOWN \
                       ATOMIC_EXECUTE_AMBIGUOUS USDCX_OUTCOME_UNKNOWN USDCX_BURN_PENDING DUPLICATE_COMMAND \
                       SUBMISSION_ALREADY_IN_FLIGHT Command already submitted USDCX_NOT_SUBMITTED transport error";
        let refusals = |text: &str| -> [anyhow::Error; 3] {
            [
                anyhow::Error::new(PackageRefusal::Unreadable(text.into())),
                anyhow::Error::new(ContentRefusal::BadRequest(text.into())),
                anyhow::Error::new(SignatureRefusal::Packages(text.into())).context("refused before signing"),
            ]
        };
        for refusal in refusals(markers) {
            let shown = format!("{refusal:#}");
            assert!(shown.contains("EXECUTE_OUTCOME_UNKNOWN") && shown.contains("USDCX_OUTCOME_UNKNOWN"), "{shown}");
            assert!(matches!(classify(Err(refusal), true), Submitted::Rejected(_)), "{shown}");
        }
        for refusal in refusals(markers) {
            assert!(matches!(classify(Err(refusal), false), Submitted::NotSigned(_)));
        }
        // An earlier execute that may still commit is re-raised as a fresh, untyped error.
        for refusal in refusals("the request is not this agent's") {
            let marked = crate::ledger_client::mark_uncertain(refusal, Some("c"));
            assert!(matches!(classify(Err(marked), true), Submitted::Unknown(_)));
        }
    }

    #[test]
    fn a_later_refusal_never_closes_a_burn_an_earlier_signature_may_commit() {
        use crate::control::intent::{ContentRefusal, PackageRefusal, SignatureRefusal};
        // Reasons that, from this attempt alone, read as a definite rejection.
        for text in [
            "Transaction failed: INSUFFICIENT_FUNDS",
            "Transaction failed: USDCX_NOT_SUBMITTED: row",
            "the request is not this agent's",
        ] {
            let refusals: [anyhow::Error; 3] = [
                anyhow::Error::new(PackageRefusal::Unreadable(text.into())),
                anyhow::Error::new(ContentRefusal::BadRequest(text.into())),
                anyhow::Error::new(SignatureRefusal::Packages(text.into())).context("refused before signing"),
            ];
            for refusal in refusals {
                // Attempt 1 signed and its execute may still commit; attempt 2 was refused before
                // signing. The guarded submit hands the refusal back as it does then.
                let delivered = crate::ledger_client::mark_uncertain(refusal, Some("usdcx-burn-attempt-1"));
                let shown = format!("{delivered:#}");
                // Unknown runs the ledger check (settle_until_known); Rejected would mark the burn
                // Failed and NotSigned would end the command with no check.
                assert!(matches!(classify(Err(delivered), true), Submitted::Unknown(_)), "{shown}");
            }
        }
        // With no earlier execute in doubt, the refusal leaves this attempt unsigned.
        let alone = anyhow::Error::new(PackageRefusal::Unreadable("Transaction failed: X".into()));
        let delivered = crate::ledger_client::mark_uncertain(alone, None);
        assert!(matches!(classify(Err(delivered), false), Submitted::NotSigned(_)));
    }
}
