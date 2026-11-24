//! Deposits: Ethereum USDC → Canton USDCx.
//!
//! `--print-only` prints DA's deposit page and the two transactions to send
//! from a wallet. Otherwise the transactions are signed here with a key typed
//! at a hidden prompt (never stored): the deposit is recorded before anything
//! is sent, signed, persisted (hash, nonce and the signed bytes), and only
//! then broadcast, so it is never lost and never sent twice. On Canton, DA
//! either mints it directly (pre-approval) or creates a DepositAttestation
//! that the party mints.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::time::Duration;

use agent_logic::clock;
use agent_logic::secret::Zeroizing;
use anyhow::{Context, Result, anyhow, bail, ensure};
use orderbook_proto::ledger::{
    PrepareTransactionRequest, TransactionOperation, UsdcxMintParams,
    prepare_transaction_request::Params,
};
use rust_decimal::Decimal;
use tx_verifier::OperationExpectation;
use tx_verifier::usdcx::mint_command_id;

use super::acs::{self, Agreement, DepositAttestation};
use super::chain::{CANTON_DOMAIN, DEPOSIT_UI_URL};
use super::eth::{
    AlloyChain, EthChain, RECEIPT_TIMEOUT, ReceiptInfo, approve_calldata, deposit_calldata,
    is_not_sent,
};
use super::ethaddr::{
    addr, hex0x, lower, parse_eth_address, party_hash, same_hex, units_to_decimal, usdc_units,
};
use super::state::{DEPOSITS_FILE, DepositEntry, DepositLog, DepositStatus, LogLock};
use super::{Ctx, burn, in_doubt, pending};
use crate::ledger_client::SubmitOptions;

const CANTON_POLL: Duration = Duration::from_secs(30);

pub(crate) struct DepositArgs {
    pub amount: String,
    pub print_only: bool,
    pub from: Option<String>,
    pub no_mint: bool,
    pub timeout: Duration,
}

/// The Ethereum chain, when `ETHEREUM_RPC_URL` is set.
async fn chain(ctx: &Ctx) -> Result<Option<AlloyChain>> {
    match std::env::var("ETHEREUM_RPC_URL") {
        Ok(url) if !url.trim().is_empty() => {
            Ok(Some(AlloyChain::connect(ctx.eth, url.trim()).await?))
        }
        _ => Ok(None),
    }
}

/// Where deposits to the party go (also shown by `usdcx status`).
pub(crate) fn print_deposit_target(ctx: &Ctx, agreement: Option<&Agreement>) {
    agent_logic::out!("Deposits to {} ({}):", ctx.party, ctx.eth.eth_chain_name);
    agent_logic::out!("  DA's deposit page:  {DEPOSIT_UI_URL}");
    agent_logic::out!("  xReserve:           {}", ctx.eth.xreserve);
    agent_logic::out!("  USDC:               {}", ctx.eth.usdc);
    agent_logic::out!("  remoteDomain:       {CANTON_DOMAIN} (Canton)");
    agent_logic::out!("  remoteRecipient:    {}", party_hash(&ctx.party));
    if let Some(a) = agreement {
        agent_logic::out!(
            "  then:               {}",
            if a.pre_approval {
                "DA mints the USDCx directly (pre-approval)"
            } else {
                "mint the DepositAttestation with `usdcx mint`"
            }
        );
    }
}

/// `usdcx deposit`.
pub(crate) async fn deposit(
    ctx: &mut Ctx,
    args: DepositArgs,
    eth_key: Option<Zeroizing<String>>,
) -> Result<()> {
    let amount = burn::parse_amount(&args.amount)?;
    let value = usdc_units(amount)?;
    let eth = ctx.eth;
    let agreement = ctx.reader().require_agreement(eth).await?;
    if args.print_only {
        return print_instructions(ctx, &agreement, amount, args.from.as_deref()).await;
    }
    let party = ctx.party.clone();
    if ctx.dry_run {
        agent_logic::out!(
            "(dry run) would send {amount} USDC from Ethereum to {party}: approve if needed, then \
             depositToRemote; nothing is signed or sent"
        );
        return Ok(());
    }
    let key = eth_key.ok_or_else(|| anyhow!("no Ethereum key was typed for this deposit"))?;
    let chain = chain(ctx)
        .await?
        .ok_or_else(|| anyhow!("ETHEREUM_RPC_URL is required (or use --print-only)"))?;

    let lock = LogLock::acquire(&ctx.dir, DEPOSITS_FILE).await?;
    let mut log = DepositLog::load(&ctx.dir)?;
    // Never a second deposit while one this agent signed may still be mined.
    if let Some(open) = log.open_unmined().cloned() {
        advance_ethereum(ctx, &chain, &mut log, &open.id).await?;
        if let Some(still) = log.open_unmined() {
            bail!(
                "deposit {} ({} USDC, tx {}) may still be mined: finish it with `usdcx track` \
                 before sending another",
                still.id,
                still.amount,
                still.tx_hash.as_deref().unwrap_or("not signed")
            );
        }
    }

    let signer = chain.signer(key.as_str()).await?;
    drop(key);
    let from = signer.from();
    for warning in chain.preflight(&party).await? {
        agent_logic::out!("⚠ {warning}");
    }
    let balance = chain.usdc_balance(from).await?;
    ensure!(
        balance >= value,
        "{} holds {} USDC; the deposit needs {amount}",
        lower(from),
        units_to_decimal(balance)?
    );
    let needs_approve = chain.usdc_allowance(from).await? < value;
    let (max_gas_wei, max_gas_eth) = chain.max_gas_cost(needs_approve).await?;
    ensure!(
        chain.eth_balance(from).await? >= max_gas_wei,
        "{} may not have enough ETH for gas (needs up to ~{max_gas_eth} ETH)",
        lower(from)
    );
    let mint_after = !agreement.pre_approval && !args.no_mint;
    agent_logic::out!(
        "=== USDCx deposit: {amount} USDC -> Canton ({}) ===",
        ctx.net.name
    );
    agent_logic::out!("  From (Ethereum):   {}", lower(from));
    agent_logic::out!("  To (Canton party): {party}");
    agent_logic::out!("  remoteRecipient:   {}", party_hash(&party));
    agent_logic::out!(
        "  xReserve:          {} on {} (chain {})",
        ctx.eth.xreserve,
        ctx.eth.eth_chain_name,
        ctx.eth.eth_chain_id
    );
    agent_logic::out!(
        "  Transactions:      {}depositToRemote",
        if needs_approve { "approve + " } else { "" }
    );
    agent_logic::out!("  Max gas cost:      ~{max_gas_eth} ETH");
    agent_logic::out!(
        "  Then:              {}",
        if agreement.pre_approval {
            "DA mints the USDCx directly after ~15-20 min of Ethereum finality"
        } else if mint_after {
            "wait ~15-20 min for the DepositAttestation, then mint it"
        } else {
            "mint later with `usdcx mint`"
        }
    );
    ctx.confirm(
        true,
        &format!("Send on {}", ctx.eth.eth_chain_name),
        &format!("{amount} USDC to {party}"),
    )
    .await?;

    // The entry comes first: its block and offset bound the later searches.
    let id = clock::uuid_v7()?.to_string();
    log.deposits.push(DepositEntry {
        id: id.clone(),
        amount: amount.to_string(),
        depositor: lower(from),
        signed_here: true,
        eth_block: chain.block_number().await?,
        ledger_offset: ctx.client.get_ledger_end().await?,
        approve_tx: None,
        tx_hash: None,
        tx_nonce: None,
        raw_tx: None,
        deposit_nonce: None,
        message_hash: None,
        attestation_cid: None,
        update_id: None,
        holding_cid: None,
        status: DepositStatus::Started,
        error: None,
        created_at: clock::now_utc(),
    });
    log.save(&ctx.dir)?;
    agent_logic::out!("  Deposit:           {id}");

    if needs_approve {
        // No deposit is signed until the approval is mined: its errors are final.
        let signed = match signer
            .sign_call(addr(ctx.eth.usdc), approve_calldata(ctx.eth, value))
            .await
        {
            Ok(s) => s,
            Err(e) => return fail_unsent(ctx, &mut log, &id, "approve not signed", e),
        };
        let hash = signed.hash;
        if let Some(entry) = log.get_mut(&id) {
            entry.approve_tx = Some(hash.to_string());
        }
        log.save(&ctx.dir)?;
        agent_logic::out!("  approve:           {}{hash}", ctx.eth.explorer_tx);
        let mined = match signer.broadcast(signed).await {
            Ok(()) => chain.wait_receipt(hash, RECEIPT_TIMEOUT).await,
            Err(e) => Err(e),
        };
        match mined {
            Ok(r) if r.success => {}
            Ok(_) => {
                return fail_unsent(
                    ctx,
                    &mut log,
                    &id,
                    "approve reverted",
                    anyhow!("approve {hash} reverted"),
                );
            }
            Err(e) => {
                return fail_unsent(
                    ctx,
                    &mut log,
                    &id,
                    "approve not confirmed; no deposit was signed",
                    e,
                );
            }
        }
    }

    // Sign, persist hash + nonce + signed bytes, THEN broadcast.
    let signed = match signer
        .sign_call(
            addr(ctx.eth.xreserve),
            deposit_calldata(ctx.eth, value, &party),
        )
        .await
    {
        Ok(s) => s,
        Err(e) => return fail_unsent(ctx, &mut log, &id, "deposit not signed; nothing sent", e),
    };
    let hash = signed.hash;
    if let Some(entry) = log.get_mut(&id) {
        entry.tx_hash = Some(hash.to_string());
        entry.tx_nonce = Some(signed.nonce);
        entry.raw_tx = Some(hex0x(signed.raw()));
        entry.status = DepositStatus::Sent;
    }
    log.save(&ctx.dir)
        .context("the deposit was signed but could not be recorded, so it was not broadcast")?;
    agent_logic::out!("  depositToRemote:   {}{hash}", ctx.eth.explorer_tx);
    let mined = match signer.broadcast(signed).await {
        Ok(()) => chain.wait_receipt(hash, RECEIPT_TIMEOUT).await,
        // The node refused it outright: it is in no pool and can never be mined.
        Err(e) if is_not_sent(&e) => {
            return fail_unsent(ctx, &mut log, &id, "deposit not broadcast", e);
        }
        Err(e) => Err(e),
    };
    let receipt = match mined {
        Ok(r) => r,
        Err(e) => {
            return Err(pending(format!(
                "deposit {id} ({hash}) may be on its way ({e:#}) — do NOT send it again; \
                 continue with `usdcx track --wait`"
            )));
        }
    };
    record_receipt(ctx, &mut log, &id, &receipt)?;
    drop(lock);
    let entry = log.deposits.iter().find(|d| d.id == id).cloned();
    match entry {
        Some(e) if e.status == DepositStatus::Failed => bail!(
            "deposit {hash} did not deposit to Canton: {}",
            e.error.unwrap_or_default()
        ),
        _ => {}
    }
    follow_to_canton(ctx, &id, !args.no_mint, args.timeout).await
}

/// Marks a deposit that certainly sent nothing to xReserve as failed.
fn fail_unsent(
    ctx: &Ctx,
    log: &mut DepositLog,
    id: &str,
    why: &str,
    e: anyhow::Error,
) -> Result<()> {
    if let Some(entry) = log.get_mut(id) {
        entry.status = DepositStatus::Failed;
        entry.error = Some(format!("{why}: {e:#}"));
    }
    log.save(&ctx.dir)?;
    Err(e.context(why.to_string()))
}

/// Records a mined deposit: its event must name the party and the amount.
fn record_receipt(ctx: &Ctx, log: &mut DepositLog, id: &str, receipt: &ReceiptInfo) -> Result<()> {
    let Some(entry) = log.get_mut(id) else {
        return Ok(());
    };
    match receipt.deposit() {
        Ok(observed) => {
            let checked = observed
                .intent
                .verify_for(ctx.eth, &ctx.party)
                .and_then(|()| {
                    let units = usdc_units(
                        entry
                            .amount
                            .parse::<Decimal>()
                            .map_err(|e| anyhow!("recorded amount: {e}"))?,
                    )?;
                    ensure!(
                        observed.intent.amount == units,
                        "the deposited value differs from the deposit"
                    );
                    Ok(())
                });
            match checked {
                Ok(()) => {
                    entry.status = DepositStatus::Confirmed;
                    entry.deposit_nonce = Some(observed.intent.nonce.to_string());
                    entry.message_hash = Some(observed.intent.message_hash().to_string());
                    agent_logic::out!(
                        "✓ Mined in block {} (deposit nonce {})",
                        observed.block_number,
                        observed.intent.nonce
                    );
                }
                Err(e) => {
                    entry.status = DepositStatus::Failed;
                    entry.error = Some(format!("{e:#}"));
                }
            }
        }
        Err(e) => {
            entry.status = DepositStatus::Failed;
            entry.error = Some(format!("{e:#}"));
        }
    }
    log.save(&ctx.dir)
}

/// Moves a sent deposit on from what Ethereum says: mined, still pending,
/// replaced, or unknown to the node (then rebroadcast from the signed bytes).
async fn advance_ethereum(
    ctx: &Ctx,
    chain: &AlloyChain,
    log: &mut DepositLog,
    id: &str,
) -> Result<()> {
    let Some(entry) = log.deposits.iter().find(|d| d.id == id).cloned() else {
        return Ok(());
    };
    match entry.status {
        DepositStatus::Started => {
            // Recorded but never signed (an approve may have been sent).
            if let Some(e) = log.get_mut(id) {
                e.status = DepositStatus::Failed;
                e.error = Some("no deposit transaction was signed".to_string());
            }
            log.save(&ctx.dir)?;
        }
        DepositStatus::Sent | DepositStatus::Confirmed => {
            let Some(hash) = entry.tx_hash.as_deref() else {
                return Ok(());
            };
            let hash: alloy_primitives::B256 = hash.parse().context("recorded tx hash")?;
            match chain.receipt(hash).await? {
                Some(receipt) => {
                    let before = entry.deposit_nonce.clone();
                    record_receipt(ctx, log, id, &receipt)?;
                    let after = log
                        .deposits
                        .iter()
                        .find(|d| d.id == id)
                        .and_then(|d| d.deposit_nonce.clone());
                    if before.is_some() && before != after {
                        agent_logic::out!(
                            "  The deposit's receipt changed (reorg): nonce {after:?}"
                        );
                    }
                }
                None if entry.status == DepositStatus::Confirmed => {
                    // A reorg dropped the receipt: it may be mined again.
                    if let Some(e) = log.get_mut(id) {
                        e.status = DepositStatus::Sent;
                    }
                    log.save(&ctx.dir)?;
                }
                None => {
                    if !chain.tx_known(hash).await? {
                        let owner = parse_eth_address(&entry.depositor)?;
                        let mined = chain.mined_nonce(owner).await?;
                        match entry.tx_nonce {
                            Some(n) if mined > n => {
                                // Its nonce was used by another transaction.
                                if let Some(e) = log.get_mut(id) {
                                    e.status = DepositStatus::Failed;
                                    e.error = Some(format!(
                                        "nonce {n} was used by another transaction: this deposit was replaced and sent nothing"
                                    ));
                                }
                                log.save(&ctx.dir)?;
                            }
                            _ => match entry.raw_tx.as_deref() {
                                Some(raw) => {
                                    agent_logic::out!(
                                        "  The node does not know {hash}: rebroadcasting it"
                                    );
                                    chain.broadcast_raw(raw).await?;
                                }
                                None => agent_logic::out!("  The node does not know {hash} yet"),
                            },
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// The DepositAttestation of a deposit: exactly the one whose reference is
/// its deposit nonce; else one of its amount from its depositor whose
/// reference is not another deposit's nonce.
fn attestation_for<'a>(
    entry: &DepositEntry,
    attestations: &'a [DepositAttestation],
    claimed: &[String],
) -> Option<&'a DepositAttestation> {
    let unclaimed = || attestations.iter().filter(|a| !claimed.contains(&a.cid));
    if let Some(nonce) = entry.deposit_nonce.as_deref()
        && let Some(a) = unclaimed().find(|a| same_hex(&a.reference, nonce))
    {
        return Some(a);
    }
    let amount = entry.amount.parse::<Decimal>().ok()?;
    let mut candidates = unclaimed().filter(|a| {
        a.amount == amount
            && a.depositor
                .as_deref()
                .is_some_and(|d| same_hex(d, &entry.depositor))
            && !looks_like_nonce(&a.reference)
    });
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

fn looks_like_nonce(reference: &str) -> bool {
    let hex = reference.strip_prefix("0x").unwrap_or(reference);
    hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Follows a confirmed deposit on Canton: DA's direct mint, or its
/// DepositAttestation and (unless `mint` is false) the mint.
async fn follow_to_canton(ctx: &mut Ctx, id: &str, mint: bool, timeout: Duration) -> Result<()> {
    let deadline = clock::deadline_after(timeout);
    loop {
        let done = canton_pass(ctx, id, mint).await?;
        if done {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(pending(format!(
                "deposit {id} is on Ethereum; it has not reached Canton yet — continue with \
                 `usdcx track --wait`"
            )));
        }
        tokio::time::sleep(CANTON_POLL).await;
    }
}

/// One look at Canton for a deposit; true when it needs nothing more.
async fn canton_pass(ctx: &mut Ctx, id: &str, mint: bool) -> Result<bool> {
    let log = DepositLog::load(&ctx.dir)?;
    let Some(entry) = log.deposits.iter().find(|d| d.id == id).cloned() else {
        return Ok(true);
    };
    if entry.status.is_terminal() {
        return Ok(true);
    }
    if !matches!(
        entry.status,
        DepositStatus::Confirmed | DepositStatus::Attested
    ) {
        return Ok(false);
    }
    let claimed: Vec<String> = log
        .deposits
        .iter()
        .filter(|d| d.id != id)
        .filter_map(|d| d.attestation_cid.clone())
        .collect();
    let used_updates: Vec<String> = log
        .deposits
        .iter()
        .filter(|d| d.id != id)
        .filter_map(|d| d.update_id.clone())
        .collect();
    let attestations = ctx.reader().deposit_attestations().await?;
    if let Some(att) = attestation_for(&entry, &attestations, &claimed).cloned() {
        update(ctx, id, |d| {
            d.status = DepositStatus::Attested;
            d.attestation_cid = Some(att.cid.clone());
        })
        .await?;
        if !mint {
            agent_logic::out!(
                "✓ DepositAttestation {} is ready: mint it with `usdcx mint --attestation-cid {}`",
                att.cid,
                att.cid
            );
            return Ok(true);
        }
        mint_one(ctx, &att).await?;
        return Ok(true);
    }
    // Pre-approval: DA mints directly, without an attestation.
    let amount = entry
        .amount
        .parse::<Decimal>()
        .map_err(|e| anyhow!("recorded amount: {e}"))?;
    let scan = acs::scan(
        &mut ctx.client,
        entry.ledger_offset,
        &acs::holding_filters(),
    )
    .await?;
    if let Some((update_id, holding)) = acs::find_direct_mint(
        &scan.transactions,
        ctx.net,
        &ctx.party,
        amount,
        &used_updates,
    ) {
        update(ctx, id, |d| {
            d.status = DepositStatus::Minted;
            d.update_id = Some(update_id.clone());
            d.holding_cid = Some(holding.clone());
        })
        .await?;
        agent_logic::out!(
            "✓ DA minted {amount} USDCx directly (update {update_id}, holding {holding})"
        );
        return Ok(true);
    }
    Ok(false)
}

/// Applies `change` to one deposit entry under the log's lock.
async fn update(ctx: &Ctx, id: &str, change: impl FnOnce(&mut DepositEntry)) -> Result<()> {
    let _lock = LogLock::acquire(&ctx.dir, DEPOSITS_FILE).await?;
    let mut log = DepositLog::load(&ctx.dir)?;
    if let Some(entry) = log.get_mut(id) {
        change(entry);
        log.save(&ctx.dir)?;
    }
    Ok(())
}

/// Mints one attestation (the command id is keyed on it, so retries cannot
/// mint twice) and records it on the deposit that claimed it.
async fn mint_one(ctx: &mut Ctx, att: &DepositAttestation) -> Result<()> {
    let expectation = OperationExpectation::UsdcxMint {
        party: ctx.party.clone(),
        network: ctx.net.name.to_string(),
        deposit_attestation_cid: att.cid.clone(),
        command_id: mint_command_id(&att.cid),
    };
    let request = PrepareTransactionRequest {
        operation: TransactionOperation::UsdcxMint as i32,
        params: Some(Params::UsdcxMint(UsdcxMintParams {
            deposit_attestation_cid: att.cid.clone(),
        })),
        request_signature: None,
        controller_command: None,
    };
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
            let holding = r.contract_id.clone().unwrap_or_default();
            agent_logic::out!(
                "✓ Minted {} USDCx (update {}, holding {})",
                att.amount,
                r.update_id,
                if holding.is_empty() { "-" } else { &holding }
            );
            let cid = att.cid.clone();
            let _lock = LogLock::acquire(&ctx.dir, DEPOSITS_FILE).await?;
            let mut log = DepositLog::load(&ctx.dir)?;
            if let Some(d) = log
                .deposits
                .iter_mut()
                .find(|d| d.attestation_cid.as_deref() == Some(cid.as_str()))
            {
                d.status = DepositStatus::Minted;
                d.update_id = Some(r.update_id.clone());
                d.holding_cid = Some(holding).filter(|h| !h.is_empty());
                log.save(&ctx.dir)?;
            }
            Ok(())
        }
        Err(e) if in_doubt(&e) => Err(pending(format!(
            "the mint of {} may have committed ({e:#}); run `usdcx mint --attestation-cid {}` \
             again: it cannot mint twice",
            att.cid, att.cid
        ))),
        Err(e) => {
            let text = format!("{e:#}");
            if text.contains("INACTIVE_CONTRACTS") || text.contains("CONTRACT_NOT_FOUND") {
                let still_there = ctx
                    .reader()
                    .deposit_attestations()
                    .await?
                    .iter()
                    .any(|a| a.cid == att.cid);
                if !still_there {
                    agent_logic::out!("  {} is no longer active: it was minted already", att.cid);
                    return Ok(());
                }
            }
            Err(e)
        }
    }
}

/// `usdcx mint`.
pub(crate) async fn mint(
    ctx: &mut Ctx,
    attestation_cid: Option<String>,
    wait: bool,
    timeout: Duration,
) -> Result<()> {
    let eth = ctx.eth;
    let agreement = ctx.reader().require_agreement(eth).await?;
    let deadline = clock::deadline_after(timeout);
    loop {
        let attestations = ctx.reader().deposit_attestations().await?;
        let chosen = match attestation_cid.as_deref() {
            Some(cid) => attestations.iter().find(|a| a.cid == cid),
            None if attestations.len() > 1 => bail!(
                "{} DepositAttestations are pending; choose one with --attestation-cid: {}",
                attestations.len(),
                attestations
                    .iter()
                    .map(|a| a.cid.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None => attestations.first(),
        };
        if let Some(att) = chosen.cloned() {
            agent_logic::out!(
                "Minting {} USDCx from DepositAttestation {}",
                att.amount,
                att.cid
            );
            ctx.confirm(false, "Mint USDCx", &att.cid).await?;
            return mint_one(ctx, &att).await;
        }
        if !wait {
            if agreement.pre_approval {
                bail!("nothing to mint: pre-approval is on, so DA mints deposits directly");
            }
            bail!(
                "no DepositAttestation to mint{}",
                match attestation_cid {
                    Some(cid) => format!(" ({cid} is not pending for {})", ctx.party),
                    None => String::new(),
                }
            );
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("no DepositAttestation appeared within the wait");
        }
        tokio::time::sleep(CANTON_POLL).await;
    }
}

/// `usdcx track` for deposits.
pub(crate) async fn track(ctx: &mut Ctx, wait: bool, timeout: Duration) -> Result<()> {
    let open: Vec<DepositEntry> = DepositLog::load(&ctx.dir)?.open().cloned().collect();
    if open.is_empty() {
        agent_logic::out!("No open deposits");
        return Ok(());
    }
    let chain = chain(ctx).await?;
    let deadline = clock::deadline_after(if wait { timeout } else { Duration::ZERO });
    loop {
        let mut still_open = 0usize;
        for entry in DepositLog::load(&ctx.dir)?
            .open()
            .cloned()
            .collect::<Vec<_>>()
        {
            agent_logic::out!(
                "--- deposit {} ({} USDC from {}, {:?})",
                entry.id,
                entry.amount,
                entry.depositor,
                entry.status
            );
            if matches!(
                entry.status,
                DepositStatus::Started | DepositStatus::Sent | DepositStatus::Confirmed
            ) {
                match &chain {
                    Some(c) => {
                        let _lock = LogLock::acquire(&ctx.dir, DEPOSITS_FILE).await?;
                        let mut log = DepositLog::load(&ctx.dir)?;
                        advance_ethereum(ctx, c, &mut log, &entry.id).await?;
                    }
                    None => agent_logic::out!(
                        "  (ETHEREUM_RPC_URL is not set: the Ethereum side is not checked)"
                    ),
                }
            }
            if !canton_pass(ctx, &entry.id, true).await? {
                still_open = still_open.saturating_add(1);
            }
        }
        if still_open == 0 {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(pending(format!("{still_open} deposit(s) still open")));
        }
        tokio::time::sleep(CANTON_POLL).await;
    }
}

/// `usdcx track --eth-tx`: records a deposit sent from a wallet.
pub(crate) async fn register_wallet_deposit(ctx: &mut Ctx, tx_hash: &str) -> Result<()> {
    let chain = chain(ctx)
        .await?
        .ok_or_else(|| anyhow!("--eth-tx needs ETHEREUM_RPC_URL to read the deposit"))?;
    let hash: alloy_primitives::B256 = tx_hash
        .trim()
        .parse()
        .context("--eth-tx is not a transaction hash")?;
    let receipt = chain
        .receipt(hash)
        .await?
        .ok_or_else(|| anyhow!("{hash} is not mined yet; register it once it is"))?;
    let observed = receipt.deposit()?.clone();
    observed.intent.verify_for(ctx.eth, &ctx.party)?;
    let amount = units_to_decimal(observed.intent.amount)?;
    let depositor = observed
        .intent
        .local_depositor_address()
        .map(lower)
        .unwrap_or_default();
    let _lock = LogLock::acquire(&ctx.dir, DEPOSITS_FILE).await?;
    let mut log = DepositLog::load(&ctx.dir)?;
    let hash_text = hash.to_string();
    if log
        .deposits
        .iter()
        .any(|d| d.tx_hash.as_deref() == Some(hash_text.as_str()))
    {
        agent_logic::out!("Deposit {hash} is already tracked");
        return Ok(());
    }
    let id = clock::uuid_v7()?.to_string();
    log.deposits.push(DepositEntry {
        id: id.clone(),
        amount: amount.to_string(),
        depositor,
        signed_here: false,
        eth_block: observed.block_number.saturating_sub(1),
        ledger_offset: ctx.client.get_ledger_end().await?,
        approve_tx: None,
        tx_hash: Some(hash_text),
        tx_nonce: None,
        raw_tx: None,
        deposit_nonce: Some(observed.intent.nonce.to_string()),
        message_hash: Some(observed.intent.message_hash().to_string()),
        attestation_cid: None,
        update_id: None,
        holding_cid: None,
        status: DepositStatus::Confirmed,
        error: None,
        created_at: clock::now_utc(),
    });
    log.save(&ctx.dir)?;
    agent_logic::out!("✓ Tracking deposit {id}: {amount} USDC ({hash})");
    Ok(())
}

/// `usdcx deposit --print-only`.
async fn print_instructions(
    ctx: &Ctx,
    agreement: &Agreement,
    amount: Decimal,
    from: Option<&str>,
) -> Result<()> {
    let party = ctx.party.as_str();
    let value = usdc_units(amount)?;
    let mut approve_needed = true;
    match chain(ctx).await? {
        Some(chain) => {
            for warning in chain.preflight(party).await? {
                agent_logic::out!("⚠ {warning}");
            }
            if let Some(from) = from {
                let owner = parse_eth_address(from)?;
                let balance = chain.usdc_balance(owner).await?;
                ensure!(
                    balance >= value,
                    "{from} holds {} USDC; the deposit needs {amount}",
                    units_to_decimal(balance)?
                );
                approve_needed = chain.usdc_allowance(owner).await? < value;
            }
        }
        None if from.is_some() => agent_logic::out!(
            "⚠ ETHEREUM_RPC_URL is not set: --from's balance and allowance are not checked"
        ),
        None => {}
    }
    agent_logic::out!(
        "=== Deposit {amount} USDC from your own wallet to Canton ({}) ===",
        ctx.net.name
    );
    agent_logic::out!("  Canton party:  {party}");
    agent_logic::out!(
        "  Onboarded:     {} (preApproval {})",
        agreement.cid,
        agreement.pre_approval
    );
    agent_logic::out!(
        "  Deposit page:  {DEPOSIT_UI_URL} (connect your wallet, enter the party above)"
    );
    agent_logic::out!();
    agent_logic::out!(
        "  ⚠ Send ONLY on {} (chain id {}). The same xReserve address exists on other chains.",
        ctx.eth.eth_chain_name,
        ctx.eth.eth_chain_id
    );
    agent_logic::out!(
        "  ⚠ Do NOT transfer USDC directly to the xReserve address: a plain transfer carries no"
    );
    agent_logic::out!(
        "    Canton recipient and does not reach Canton. Use the contract calls below."
    );
    agent_logic::out!();
    let mut step = 1u8;
    if approve_needed {
        agent_logic::out!("  Step {step}: approve USDC (exact amount)");
        agent_logic::out!("    to:     {} (USDC)", ctx.eth.usdc);
        agent_logic::out!("    value:  0");
        agent_logic::out!("    data:   {}", hex0x(&approve_calldata(ctx.eth, value)));
        agent_logic::out!(
            "    = approve(spender={}, amount={value})",
            ctx.eth.xreserve
        );
        agent_logic::out!();
        step = step.saturating_add(1);
    }
    agent_logic::out!("  Step {step}: deposit into xReserve");
    agent_logic::out!("    to:     {} (xReserve)", ctx.eth.xreserve);
    agent_logic::out!("    value:  0");
    agent_logic::out!(
        "    data:   {}",
        hex0x(&deposit_calldata(ctx.eth, value, party))
    );
    agent_logic::out!(
        "    = depositToRemote(value={value}, remoteDomain={CANTON_DOMAIN}, remoteRecipient={}, \
         localToken={}, maxFee=0, hookData=<the party id as bytes>)",
        party_hash(party),
        ctx.eth.usdc
    );
    agent_logic::out!();
    agent_logic::out!(
        "  Then follow it with `usdcx track --eth-tx <deposit tx hash> --wait`{}",
        if agreement.pre_approval {
            " (DA mints directly)"
        } else {
            ", which mints it"
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(amount: &str, depositor: &str, nonce: Option<&str>) -> DepositEntry {
        DepositEntry {
            id: "d1".into(),
            amount: amount.into(),
            depositor: depositor.into(),
            signed_here: true,
            eth_block: 1,
            ledger_offset: 1,
            approve_tx: None,
            tx_hash: None,
            tx_nonce: None,
            raw_tx: None,
            deposit_nonce: nonce.map(str::to_string),
            message_hash: None,
            attestation_cid: None,
            update_id: None,
            holding_cid: None,
            status: DepositStatus::Confirmed,
            error: None,
            created_at: clock::now_utc(),
        }
    }

    fn att(cid: &str, amount: &str, depositor: &str, reference: &str) -> DepositAttestation {
        DepositAttestation {
            cid: cid.into(),
            amount: amount.parse().unwrap(),
            depositor: Some(depositor.into()),
            reference: reference.into(),
        }
    }

    const ME: &str = "0x1111111111111111111111111111111111111111";
    const NONCE: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_NONCE: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn an_attestation_is_matched_by_nonce_first() {
        let e = entry("10", ME, Some(NONCE));
        let list = vec![att("c1", "10", ME, OTHER_NONCE), att("c2", "10", ME, NONCE)];
        assert_eq!(attestation_for(&e, &list, &[]).unwrap().cid, "c2");
        // Claimed by another deposit: never ours.
        assert!(attestation_for(&e, &list, &["c2".into()]).is_none());
    }

    #[test]
    fn without_a_nonce_match_only_one_unambiguous_candidate_counts() {
        let e = entry("10", ME, None);
        // Another deposit's nonce in the reference: never ours.
        assert!(attestation_for(&e, &[att("c1", "10", ME, OTHER_NONCE)], &[]).is_none());
        assert_eq!(
            attestation_for(&e, &[att("c1", "10", ME, "")], &[])
                .unwrap()
                .cid,
            "c1"
        );
        // Two candidates: ambiguous.
        assert!(
            attestation_for(&e, &[att("c1", "10", ME, ""), att("c2", "10", ME, "")], &[]).is_none()
        );
        // Wrong amount or depositor.
        assert!(attestation_for(&e, &[att("c1", "11", ME, "")], &[]).is_none());
        assert!(
            attestation_for(
                &e,
                &[att(
                    "c1",
                    "10",
                    "0x2222222222222222222222222222222222222222",
                    ""
                )],
                &[]
            )
            .is_none()
        );
    }
}
