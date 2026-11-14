//! The party's bridge agreement: onboarding, pre-approval, offboarding and
//! the status overview.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::time::Duration;

use agent_logic::clock;
use anyhow::{Result, anyhow, bail, ensure};
use orderbook_proto::ledger::{
    PrepareTransactionRequest, TransactionOperation, UsdcxRequestAgreementParams,
    UsdcxTerminateAgreementParams, UsdcxUpdatePreApprovalParams,
    prepare_transaction_request::Params,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tx_verifier::OperationExpectation;
use tx_verifier::usdcx::onboard_command_id;

use super::acs::Agreement;
use super::state::{self, BurnLog, BurnStatus, DepositLog};
use super::{Ctx, deposit, in_doubt, pending};
use crate::ledger_client::SubmitOptions;

const ONBOARD_POLL: Duration = Duration::from_secs(5);
/// The onboarding request id of the party, kept so that a re-run reuses it.
const ONBOARD_FILE: &str = "onboard.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct OnboardRecord {
    #[serde(default)]
    request_id: Option<String>,
}

fn print_agreement(a: &Agreement) {
    agent_logic::out!("  Agreement:          {}", a.cid);
    agent_logic::out!("    preApproval:      {}", a.pre_approval);
    agent_logic::out!("    minBurnValue:     {} USDCx", a.min_burn_value);
    agent_logic::out!("    withdrawalAttester: {}", a.withdrawal_attester);
}

/// `usdcx onboard`: requests the agreement unless one or a request exists,
/// then waits for DA's answer.
pub(crate) async fn onboard(
    ctx: &mut Ctx,
    pre_approval: bool,
    request_id: Option<String>,
    timeout: Duration,
) -> Result<()> {
    agent_logic::out!("=== USDCx onboarding ({}) ===", ctx.net.name);
    agent_logic::out!("  Party:            {}", ctx.party);
    agent_logic::out!("  USDCx admin:      {}", ctx.net.admin);
    agent_logic::out!("  Bridge operator:  {}", ctx.net.bridge_operator);
    if let Some(a) = ctx.reader().agreement().await? {
        a.verify(ctx.eth, &ctx.party)?;
        agent_logic::out!("✓ Already onboarded");
        print_agreement(&a);
        return Ok(());
    }
    let rejected_before: Vec<String> = ctx
        .reader()
        .rejected_requests()
        .await?
        .into_iter()
        .map(|(cid, _)| cid)
        .collect();
    let pending_requests = ctx.reader().pending_requests().await?;
    if let Some(cid) = pending_requests.first() {
        agent_logic::out!("  Request {cid} is waiting for DA");
    } else {
        // An interrupted onboarding is continued under its request id: the
        // command id is keyed on it, so the ledger refuses a second request.
        let record_path = ctx.dir.join(ONBOARD_FILE);
        let record: OnboardRecord = state::load(&record_path)?;
        let request_id = match (request_id, record.request_id) {
            (Some(given), _) => given,
            (None, Some(previous)) => {
                agent_logic::out!("  Continuing the earlier onboarding request {previous}");
                previous
            }
            (None, None) => clock::uuid_v4()?.to_string(),
        };
        let parsed = uuid::Uuid::parse_str(&request_id)
            .map_err(|e| anyhow!("request id {request_id}: {e}"))?;
        ensure!(
            parsed.get_version_num() == 4,
            "request id {request_id} is not a UUID v4"
        );
        agent_logic::out!("  Request id:       {request_id}");
        agent_logic::out!("  preApproval:      {pre_approval}");
        ctx.confirm(false, "Request a USDCx bridge agreement", &ctx.party)
            .await?;
        if !ctx.dry_run {
            state::save(
                &record_path,
                &OnboardRecord {
                    request_id: Some(request_id.clone()),
                },
            )?;
        }
        let expectation = OperationExpectation::UsdcxRequestAgreement {
            party: ctx.party.clone(),
            network: ctx.net.name.to_string(),
            pre_approval,
            command_id: onboard_command_id(&ctx.party, &request_id),
        };
        let request = PrepareTransactionRequest {
            operation: TransactionOperation::UsdcxRequestAgreement as i32,
            params: Some(Params::UsdcxRequestAgreement(UsdcxRequestAgreementParams {
                pre_approval,
                request_id: request_id.clone(),
            })),
            request_signature: None,
        };
        let opts = SubmitOptions {
            max_retries: 3,
            pre_sign: None,
        };
        let result = ctx
            .client
            .submit_transaction_guarded(
                request,
                &expectation,
                ctx.verbose,
                ctx.dry_run,
                false,
                opts,
            )
            .await;
        if ctx.dry_run {
            return result
                .map(|_| agent_logic::out!("(dry run — nothing was signed or submitted)"));
        }
        match result {
            Ok(r) => agent_logic::out!(
                "  Request {} submitted (update {})",
                r.contract_id.as_deref().unwrap_or("-"),
                r.update_id
            ),
            Err(e) if in_doubt(&e) => {
                return Err(pending(format!(
                    "the onboarding request may have been submitted ({e:#}); run `usdcx onboard` \
                     again: it continues request {request_id}"
                )));
            }
            Err(e) => return Err(e),
        }
    }

    let deadline = clock::deadline_after(timeout);
    loop {
        if let Some(a) = ctx.reader().agreement().await? {
            a.verify(ctx.eth, &ctx.party)?;
            agent_logic::out!("✓ Onboarded: DA accepted the request");
            print_agreement(&a);
            let _ = std::fs::remove_file(ctx.dir.join(ONBOARD_FILE));
            return Ok(());
        }
        if let Some((cid, reason)) = ctx
            .reader()
            .rejected_requests()
            .await?
            .into_iter()
            .find(|(cid, _)| !rejected_before.contains(cid))
        {
            let _ = std::fs::remove_file(ctx.dir.join(ONBOARD_FILE));
            bail!("DA rejected the onboarding request ({reason}; {cid})");
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(pending(
                "DA has not answered the onboarding request yet; run `usdcx onboard` again to keep \
                 waiting",
            ));
        }
        tokio::time::sleep(ONBOARD_POLL).await;
    }
}

/// `usdcx pre-approval`: switches DA's direct minting of deposits.
pub(crate) async fn pre_approval(ctx: &mut Ctx, enable: bool) -> Result<()> {
    let eth = ctx.eth;
    let a = ctx.reader().require_agreement(eth).await?;
    let word = if enable { "on" } else { "off" };
    if a.pre_approval == enable {
        agent_logic::out!("Pre-approval is already {word} (agreement {})", a.cid);
        return Ok(());
    }
    ctx.confirm(
        false,
        "Change USDCx pre-approval",
        &format!("pre-approval {word}"),
    )
    .await?;
    let expectation = OperationExpectation::UsdcxUpdatePreApproval {
        party: ctx.party.clone(),
        network: ctx.net.name.to_string(),
        pre_approval: enable,
    };
    let request = PrepareTransactionRequest {
        operation: TransactionOperation::UsdcxUpdatePreApproval as i32,
        params: Some(Params::UsdcxUpdatePreApproval(
            UsdcxUpdatePreApprovalParams {
                pre_approval: enable,
            },
        )),
        request_signature: None,
    };
    // Its choice consumes the agreement: one attempt, then check the ledger.
    let opts = SubmitOptions {
        max_retries: 1,
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
                "✓ Pre-approval {word} (agreement {}, update {})",
                r.contract_id.as_deref().unwrap_or("-"),
                r.update_id
            );
            Ok(())
        }
        Err(e) if in_doubt(&e) => Err(pending(format!(
            "the change may have committed ({e:#}); check `usdcx status` and run it again only if \
             pre-approval is still {}",
            if enable { "off" } else { "on" }
        ))),
        Err(e) => Err(e),
    }
}

/// `usdcx offboard`: terminates the party's agreement.
pub(crate) async fn offboard(ctx: &mut Ctx, reason: &str) -> Result<()> {
    let eth = ctx.eth;
    let a = ctx.reader().require_agreement(eth).await?;
    let holdings = ctx.reader().holdings().await?;
    let total = holdings
        .iter()
        .try_fold(Decimal::ZERO, |sum, h| sum.checked_add(h.amount))
        .ok_or_else(|| anyhow!("balance overflow"))?;
    agent_logic::out!("Terminating agreement {} of {}", a.cid, ctx.party);
    if total > Decimal::ZERO {
        agent_logic::out!(
            "  ⚠ The party holds {total} USDCx: without an agreement it can no longer burn them"
        );
    }
    ctx.confirm(true, "Terminate the USDCx bridge agreement", &a.cid)
        .await?;
    let expectation = OperationExpectation::UsdcxTerminateAgreement {
        party: ctx.party.clone(),
        network: ctx.net.name.to_string(),
        reason: reason.to_string(),
    };
    let request = PrepareTransactionRequest {
        operation: TransactionOperation::UsdcxTerminateAgreement as i32,
        params: Some(Params::UsdcxTerminateAgreement(
            UsdcxTerminateAgreementParams {
                reason: reason.to_string(),
            },
        )),
        request_signature: None,
    };
    let opts = SubmitOptions {
        max_retries: 1,
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
            agent_logic::out!("✓ Agreement terminated (update {})", r.update_id);
            Ok(())
        }
        Err(e) if in_doubt(&e) => Err(pending(format!(
            "the termination may have committed ({e:#}); check `usdcx status` before running it \
             again"
        ))),
        Err(e) => Err(e),
    }
}

/// `usdcx status`.
pub(crate) async fn status(ctx: &mut Ctx) -> Result<()> {
    agent_logic::out!("=== USDCx status ({}) ===", ctx.net.name);
    agent_logic::out!("  Party:            {}", ctx.party);
    agent_logic::out!("  USDCx admin:      {}", ctx.net.admin);
    agent_logic::out!("  Bridge operator:  {}", ctx.net.bridge_operator);
    let agreement = ctx.reader().agreement().await?;
    match &agreement {
        Some(a) => {
            match a.verify(ctx.eth, &ctx.party) {
                Ok(()) => agent_logic::out!("✓ Onboarded"),
                Err(e) => agent_logic::out!("✗ The agreement does not verify: {e:#}"),
            }
            print_agreement(a);
        }
        None => {
            let pending_requests = ctx.reader().pending_requests().await?;
            match pending_requests.first() {
                Some(cid) => agent_logic::out!("  Onboarding request {cid} is waiting for DA"),
                None => agent_logic::out!("  Not onboarded — next: `usdcx onboard`"),
            }
            for (cid, reason) in ctx.reader().rejected_requests().await? {
                agent_logic::out!("  Rejected request {cid}: {reason}");
            }
        }
    }

    let holdings = ctx.reader().holdings().await?;
    let (mut unlocked, mut locked) = (Decimal::ZERO, Decimal::ZERO);
    for h in &holdings {
        let slot = if h.locked { &mut locked } else { &mut unlocked };
        *slot = slot
            .checked_add(h.amount)
            .ok_or_else(|| anyhow!("balance overflow"))?;
    }
    agent_logic::out!(
        "  USDCx:            {unlocked} unlocked, {locked} locked ({} holdings)",
        holdings.len()
    );

    let attestations = ctx.reader().deposit_attestations().await?;
    for a in &attestations {
        agent_logic::out!(
            "  Deposit to mint:  {} USDCx from {} ({}) — `usdcx mint --attestation-cid {}`",
            a.amount,
            a.depositor.as_deref().unwrap_or("?"),
            a.cid,
            a.cid
        );
    }
    for w in ctx.reader().withdraw_attestations().await? {
        agent_logic::out!(
            "  Withdrawal:       {} USDCx to {} — waiting for DA (request {})",
            w.amount,
            w.destination,
            w.request_id
        );
    }
    for w in ctx.reader().accepted_withdrawals().await? {
        agent_logic::out!(
            "  Released:         {} USDCx to {} (request {}, trace {})",
            w.amount,
            w.destination,
            w.request_id,
            w.detail.as_deref().unwrap_or("-")
        );
    }
    for w in ctx.reader().revoked_withdrawals().await? {
        agent_logic::out!(
            "  Revoked:          {} USDCx (request {}: {}) — `usdcx reclaim --revoked-cid {}`",
            w.amount,
            w.request_id,
            w.detail.as_deref().unwrap_or("no reason"),
            w.cid
        );
    }

    let burns = BurnLog::load(&ctx.dir)?;
    for b in burns.burns.iter().filter(|b| {
        matches!(
            b.status,
            BurnStatus::Signed | BurnStatus::Committed | BurnStatus::Revoked
        )
    }) {
        agent_logic::out!(
            "  Local withdrawal: {} {} USDCx to {} ({:?})",
            b.request_id,
            b.amount,
            b.destination,
            b.status
        );
    }
    let deposits = DepositLog::load(&ctx.dir)?;
    for d in deposits.open() {
        agent_logic::out!(
            "  Local deposit:    {} {} USDC from {} ({:?}, tx {})",
            d.id,
            d.amount,
            d.depositor,
            d.status,
            d.tx_hash.as_deref().unwrap_or("-")
        );
    }
    agent_logic::out!("  State directory:  {}", ctx.dir.display());
    agent_logic::out!();
    deposit::print_deposit_target(ctx, agreement.as_ref());
    Ok(())
}
