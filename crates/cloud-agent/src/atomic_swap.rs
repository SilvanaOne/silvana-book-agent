//! RFQ V2 (AtomicDVP) taker state machine (design §7.1).
//!
//! Analog of `accept_settle::MulticallSettler` for the atomic path: one
//! accepted envelope → H14 pre-check → own-holdings selection + reservation →
//! `PrepareAtomicTransaction(AtomicDvpSettleParams)` → tx-verify → sign →
//! `ExecuteAtomicTransaction` — ONE transaction, no proposal ladder.
//!
//! Failure classification is exactly-once disciplined: an ambiguous execute
//! failure passes through the mandatory reconciliation gate before anything
//! is released or re-quoted (see [`AtomicSwapper::settle_envelope`]).

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use rust_decimal::Decimal;
use serde_json::Value;
use tracing::{info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::confirm::ConfirmLock;
use atomic_quote::envelope::{pre_submit_check, AcsContractJson, QuoteEnvelope, QuoteJson};
use orderbook_proto::ledger::{
    prepare_transaction_request::Params as V1Params, PrepareTransactionRequest,
    RequestPreapprovalParams, TransactionOperation,
};
use orderbook_proto::rfqv2::{
    prepare_atomic_transaction_request::Params as AtomicParams, AtomicDvpSettleParams,
    AtomicQuoteEnvelope, PrepareAtomicTransactionRequest,
};
use tx_verifier::OperationExpectation;

use crate::fill_loop::FillDirection;
use crate::holdings_cache::{
    instrument_key, CachedHolding, HoldingsCache, CC_INSTRUMENT, TEMPLATE_AMULET, TEMPLATE_HOLDING,
};
use crate::ledger_client::{is_ambiguous_execute_error, AtomicProviderClient, DAppProviderClient};

/// H14 safety margin: the signed window must exceed this at pre-check time
/// (the reference CLI uses 0; agents leave room for prepare+sign+execute).
const PRECHECK_VALIDITY_MARGIN_MICROS: i64 = 10_000_000;

/// Grace beyond the signed `valid_until` for the taker's own input
/// reservations (mirrors the LP's `settle_grace_secs` default).
const RESERVATION_GRACE: Duration = Duration::from_secs(30);

/// Reconciliation gate polling: attempts x interval before concluding the
/// user's own inputs are still live.
const RECONCILE_ATTEMPTS: u32 = 3;
const RECONCILE_POLL: Duration = Duration::from_secs(10);

/// Prepare-stage transient errors are retried within the validity window,
/// bounded by this (nothing has reached the ledger yet — safe).
const MAX_PREPARE_RETRIES: u32 = 5;

/// A committed atomic fill.
#[derive(Debug, Clone)]
pub struct AtomicFill {
    pub update_id: String,
    /// Filled base quantity (the signed quote's `base_amount`).
    pub filled_base: f64,
}

/// Outcome of one envelope settle attempt.
pub enum SwapOutcome {
    /// The settle committed (directly, or established via reconciliation).
    Filled(AtomicFill),
    /// Round failed cleanly — own reservations released; request fresh quotes.
    Requote { reason: String },
    /// Commit status could not be established (reconciliation gate exhausted).
    /// The caller MUST stop: a blind re-quote after an unnoticed success is a
    /// second real settle (double fill). Own reservations are left to expire.
    Abort { reason: String },
    /// `--dry-run`: prepared + verified, never signed or executed.
    DryRun,
}

/// Taker-side driver for the atomic settle round.
pub struct AtomicSwapper {
    pub config: BaseConfig,
    /// Shared holdings cache (populated by the fill backend's ACS worker).
    pub cache: Arc<HoldingsCache>,
    /// Select the splitter reserve too (a reserve-enabled cache withholds the
    /// party's LARGEST holding per instrument for the LP ladder). The taker
    /// spends the user's own funds, so a harness sharing one cache across
    /// both roles sets this; the production fill backend's cache has the
    /// reserve off and leaves it false.
    pub spend_reserve: bool,
    pub verbose: bool,
    pub dry_run: bool,
    pub force: bool,
    pub confirm: bool,
    pub confirm_lock: ConfirmLock,
}

// ============================================================================
// Envelope conversion (proto transport → reference file-format shape)
// ============================================================================

fn acs_from_proto(c: &orderbook_proto::rfqv2::AtomicAcsContract) -> Result<AcsContractJson> {
    Ok(AcsContractJson {
        contract_id: c.contract_id.clone(),
        template_id: c.template_id.clone(),
        created_event_blob: c.created_event_blob.clone(),
        payload: serde_json::from_str(&c.payload_json)
            .with_context(|| format!("invalid payload_json on contract {}", c.contract_id))?,
    })
}

/// Proto `AtomicQuoteEnvelope` → reference [`QuoteEnvelope`] for the H14
/// pre-check. Transport-only fields (rfq_id, quote_id, lp_party_id, market_id)
/// are dropped; disclosed contracts become the canonical 4-camelCase-key shape.
pub fn envelope_from_proto(env: &AtomicQuoteEnvelope) -> Result<QuoteEnvelope> {
    let dvp = env.dvp.as_ref().ok_or_else(|| anyhow!("envelope missing dvp contract"))?;
    let quote = env.quote.as_ref().ok_or_else(|| anyhow!("envelope missing quote"))?;
    Ok(QuoteEnvelope {
        version: env.version.clone(),
        synchronizer_id: env.synchronizer_id.clone(),
        dvp: acs_from_proto(dvp)?,
        quote: QuoteJson {
            quote_id: quote.quote_id.clone(),
            ticket_id: quote.ticket_id.clone(),
            user: quote.user.clone(),
            side: quote.side.clone(),
            base_amount: quote.base_amount.clone(),
            quote_amount: quote.quote_amount.clone(),
            created_at_micros: quote.created_at_micros.to_string(),
            valid_until_micros: quote.valid_until_micros.to_string(),
            lp_fees: if quote.lp_fees.is_empty() {
                None
            } else {
                Some(
                    quote
                        .lp_fees
                        .iter()
                        .map(|f| atomic_quote::envelope::LpFeeJson {
                            receiver: f.receiver.clone(),
                            instrument_id: atomic_quote::envelope::InstrumentIdJson {
                                admin: f.instrument_admin.clone(),
                                id: f.instrument_id.clone(),
                            },
                            amount: f.amount.clone(),
                        })
                        .collect(),
                )
            },
        },
        canonical_message: env.canonical_message.clone(),
        quote_signature: env.quote_signature.clone(),
        ticket: env.ticket.as_ref().map(acs_from_proto).transpose()?,
        lp_input_holding_cids: env.lp_input_holding_cids.clone(),
        disclosed: env
            .disclosed
            .iter()
            .map(|d| {
                serde_json::json!({
                    "contractId": d.contract_id,
                    "templateId": d.template_id,
                    "createdEventBlob": d.created_event_blob,
                    "synchronizerId": d.synchronizer_id,
                })
            })
            .collect(),
    })
}

// ============================================================================
// Clients
// ============================================================================

pub async fn create_atomic_client(config: &BaseConfig) -> Result<AtomicProviderClient> {
    AtomicProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
}

pub async fn create_v1_client(config: &BaseConfig) -> Result<DAppProviderClient> {
    DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
}

// ============================================================================
// Receiver-preapproval preflight (design §6.5)
// ============================================================================

/// Ensure this party holds a utility `TransferPreapproval` for the instrument
/// it RECEIVES (registry transfer factories only return a one-step Completed
/// context when the receiver is preapproved). CC needs none (the two-step
/// in-tx accept is forced by design). Returns `true` when a preapproval was
/// created, `false` when already present / not needed.
pub async fn ensure_receiver_preapproval(
    config: &BaseConfig,
    client: &mut DAppProviderClient,
    orderbook_instrument_id: &str,
    verbose: bool,
    dry_run: bool,
    force: bool,
) -> Result<bool> {
    let (on_chain_id, admin) = config.resolve_instrument(orderbook_instrument_id);
    if on_chain_id == "Amulet" {
        return Ok(false); // CC leg: no preapproval needed
    }
    if admin.is_empty() {
        bail!(
            "cannot resolve registry admin for instrument '{}' — instrument registry not populated",
            orderbook_instrument_id
        );
    }

    let existing = client.get_preapprovals().await?;
    if existing.iter().any(|p| p.instrument_admin == admin) {
        info!(
            "Receiver preapproval for {} (admin {}) already present",
            on_chain_id, admin
        );
        return Ok(false);
    }

    // Operator resolution mirrors `run_preapproval`: match the faucet
    // instrument list by registry.
    let faucet_instruments = client
        .list_faucet_instruments()
        .await
        .context("failed to fetch faucet instruments for preapproval operator resolution")?;
    let operator = faucet_instruments
        .iter()
        .find(|inst| inst.registry == admin)
        .map(|inst| inst.operator.clone())
        .ok_or_else(|| {
            anyhow!(
                "no faucet instrument matches admin '{}' — cannot determine preapproval operator",
                admin
            )
        })?;

    info!(
        "Creating receiver preapproval for {} (admin {}, operator {})",
        on_chain_id, admin, operator
    );
    let expectation = OperationExpectation::RequestPreapproval {
        party: config.party_id.clone(),
    };
    let result = client
        .submit_transaction(
            PrepareTransactionRequest {
                operation: TransactionOperation::RequestPreapproval as i32,
                params: Some(V1Params::RequestPreapproval(RequestPreapprovalParams {
                    instrument_admin: admin,
                    instrument_allowances: vec![],
                    operator,
                })),
                request_signature: None,
            },
            &expectation,
            verbose,
            dry_run,
            force,
        )
        .await
        .context("receiver preapproval submission failed")?;
    info!("Receiver preapproval created (update {})", result.update_id);
    Ok(true)
}

// ============================================================================
// The settle round
// ============================================================================

enum ReconcileStatus {
    Consumed,
    Live,
    Unknown,
}

impl AtomicSwapper {
    /// One accepted-envelope round: H14 pre-check → select+reserve own inputs
    /// → prepare/verify/sign/execute → confirm or classify the failure.
    ///
    /// `accepted` is the indicative quote the user accepted (`AtomicQuoteInfo`);
    /// `max_input_holdings` is the per-market config cap (protocol bound 100).
    pub async fn settle_envelope(
        &self,
        envelope: &AtomicQuoteEnvelope,
        accepted: &orderbook_proto::rfqv2::AtomicQuoteInfo,
        direction: FillDirection,
        max_input_holdings: usize,
    ) -> Result<SwapOutcome> {
        // ---- H14 PRECHECK ----------------------------------------------
        let env = envelope_from_proto(envelope)?;
        let now_micros = chrono::Utc::now().timestamp_micros();
        if let Err(e) = pre_submit_check(
            &env,
            &self.config.party_id,
            &self.config.synchronizer_id,
            now_micros,
            PRECHECK_VALIDITY_MARGIN_MICROS,
        ) {
            // Nothing reserved yet — reject the quote and let the caller
            // move on to the next quote/LP.
            return Ok(SwapOutcome::Requote {
                reason: format!("H14 pre-check failed: {e:#}"),
            });
        }

        let venue = |ptr: &str| -> Result<String> {
            env.dvp
                .payload
                .pointer(ptr)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("AtomicDVP venue payload missing {ptr}"))
        };
        let lp_party = venue("/lp")?;
        if lp_party != accepted.lp_party_id {
            return Ok(SwapOutcome::Requote {
                reason: format!(
                    "H14: venue lp {} does not match the quoted LP {}",
                    lp_party, accepted.lp_party_id
                ),
            });
        }
        // Amounts must equal the accepted indicative quote (decimal compare —
        // the LP must not re-price at confirm).
        let base_amount = Decimal::from_str(&env.quote.base_amount)
            .with_context(|| format!("invalid base_amount '{}'", env.quote.base_amount))?;
        let quote_amount = Decimal::from_str(&env.quote.quote_amount)
            .with_context(|| format!("invalid quote_amount '{}'", env.quote.quote_amount))?;
        let accepted_base = Decimal::from_str(&accepted.quantity)
            .with_context(|| format!("invalid accepted quantity '{}'", accepted.quantity))?;
        let accepted_quote = Decimal::from_str(&accepted.quote_quantity)
            .with_context(|| format!("invalid accepted quote_quantity '{}'", accepted.quote_quantity))?;
        if base_amount != accepted_base || quote_amount != accepted_quote {
            return Ok(SwapOutcome::Requote {
                reason: format!(
                    "H14: signed amounts ({}, {}) differ from accepted quote ({}, {})",
                    base_amount, quote_amount, accepted_base, accepted_quote
                ),
            });
        }
        let expected_side = match direction {
            FillDirection::Buy => "Buy",
            FillDirection::Sell => "Sell",
        };
        if env.quote.side != expected_side {
            return Ok(SwapOutcome::Requote {
                reason: format!("H14: quote side {} != {}", env.quote.side, expected_side),
            });
        }
        // Fee consent (design §14 D21): the SIGNED lpFees must equal the fee
        // displayed on the accepted quote — neither the LP nor the relay can
        // raise the fee after display.
        let signed_fees = envelope
            .quote
            .as_ref()
            .map(|q| q.lp_fees.clone())
            .unwrap_or_default();
        let displayed_fees: Vec<_> = accepted.settlement_fee.iter().cloned().collect();
        if signed_fees != displayed_fees {
            return Ok(SwapOutcome::Requote {
                reason: format!(
                    "signed lpFees {:?} differ from the displayed settlement fee {:?}",
                    signed_fees, displayed_fees
                ),
            });
        }

        // ---- SELECT own holdings (sending leg) --------------------------
        // User Buy ⇒ user pays quote leg; Sell ⇒ user pays base leg.
        let (pay_admin, pay_id, amount_needed) = match direction {
            FillDirection::Buy => (
                venue("/quoteInstrumentId/admin")?,
                venue("/quoteInstrumentId/id")?,
                quote_amount,
            ),
            FillDirection::Sell => (
                venue("/baseInstrumentId/admin")?,
                venue("/baseInstrumentId/id")?,
                base_amount,
            ),
        };
        let is_cc = pay_id == "Amulet";
        let pay_key = if is_cc {
            CC_INSTRUMENT.to_string()
        } else {
            instrument_key(&pay_admin, &pay_id)
        };
        // Fee funding (design §14 D21, generalized for fee-token selection):
        // fees ride the ONE user-side pool — coverage must be GUARANTEED, not
        // incidental. Per fee instrument: CC targets fee × 1.02 + 1 CC (amulet
        // sender-fee headroom, the dvp CLI margin); utility tokens (USDC…)
        // transfer one-step via the receiver's preapproval with no
        // token-denominated sender fee, so the exact amount suffices.
        let mut fee_totals: Vec<((String, String), Decimal)> = Vec::new();
        for f in &signed_fees {
            let Ok(a) = Decimal::from_str(&f.amount) else {
                continue;
            };
            let key = (f.instrument_admin.clone(), f.instrument_id.clone());
            match fee_totals.iter_mut().find(|(k, _)| *k == key) {
                Some((_, total)) => *total += a,
                None => fee_totals.push((key, a)),
            }
        }
        let fee_target = |id: &str, total: Decimal| {
            if id == "Amulet" {
                total * Decimal::new(102, 2) + Decimal::ONE
            } else {
                total
            }
        };
        // CC is unique by id "Amulet"; utility instruments match on admin+id.
        let fee_matches = |admin: &str, id: &str, other_admin: &str, other_id: &str| {
            id == other_id && (id == "Amulet" || admin == other_admin)
        };

        // What the user RECEIVES (proceeds can fund a same-instrument fee).
        let (receive_admin, receive_id) = match direction {
            FillDirection::Buy => (
                venue("/baseInstrumentId/admin")?,
                venue("/baseInstrumentId/id")?,
            ),
            FillDirection::Sell => (
                venue("/quoteInstrumentId/admin")?,
                venue("/quoteInstrumentId/id")?,
            ),
        };
        let receive_amount = match direction {
            FillDirection::Buy => base_amount,
            FillDirection::Sell => quote_amount,
        };

        let max_inputs = max_input_holdings.min(100);
        // When a fee is denominated in the PAY leg's instrument, the leg
        // selection itself must also cover it (leg change alone can be
        // arbitrarily small).
        let pay_fee_extra: Decimal = fee_totals
            .iter()
            .filter(|((admin, id), _)| fee_matches(admin, id, &pay_admin, &pay_id))
            .map(|((_, id), total)| fee_target(id, *total))
            .sum();
        let select_target = amount_needed + pay_fee_extra;
        // Bounded re-poll (LP confirm-path parity): an empty selection is
        // often a seconds-long gap — fresh proceeds blob-pending until the
        // next ACS snapshot, holdings momentarily reserved — not depletion.
        // Wait inside the signed validity window, keeping the pre-check
        // margin for prepare+sign+execute — but only when the cache holds
        // enough in PRINCIPLE (counting stale/blob-pending entries a refresh
        // can revive): a genuinely underfunded taker must fail fast, not
        // burn the window.
        let valid_until_micros: i64 = env.quote.valid_until_micros.parse()?;
        async fn selection_deadline(
            cache: &HoldingsCache,
            key: &str,
            target: Decimal,
            valid_until_micros: i64,
        ) -> Instant {
            if cache.total_available_amount(key).await < target {
                return Instant::now();
            }
            let usable_micros = (valid_until_micros
                - PRECHECK_VALIDITY_MARGIN_MICROS
                - chrono::Utc::now().timestamp_micros())
            .max(0) as u64;
            Instant::now()
                + Duration::from_micros(usable_micros).min(crate::rfq_v2::MAX_CONFIRM_SPLIT_WAIT)
        }
        let picks = self
            .cache
            .select_for_disclosure_until(
                &pay_key,
                select_target,
                max_inputs,
                is_cc,
                self.spend_reserve,
                selection_deadline(&self.cache, &pay_key, select_target, valid_until_micros).await,
                crate::rfq_v2::CONFIRM_SPLIT_POLL,
            )
            .await;
        let Some(mut picks) = picks else {
            return Ok(SwapOutcome::Requote {
                reason: format!(
                    "own holdings selection failed for {} (need {}; cache cold or insufficient)",
                    pay_key, select_target
                ),
            });
        };
        // Fees in OTHER instruments: covered by same-instrument proceeds when
        // large enough, else add dedicated funding cids to the pool.
        for ((admin, id), total) in &fee_totals {
            if fee_matches(admin, id, &pay_admin, &pay_id) {
                continue; // already inside select_target
            }
            let target = fee_target(id, *total);
            if target <= Decimal::ZERO {
                continue;
            }
            if fee_matches(admin, id, &receive_admin, &receive_id) && receive_amount >= target {
                continue; // proceeds fund the fee
            }
            let fee_key = if id == "Amulet" {
                CC_INSTRUMENT.to_string()
            } else {
                instrument_key(admin, id)
            };
            let slots = max_inputs.saturating_sub(picks.len()).max(1);
            // Same bounded wait as the pay leg — fee cids are blob-pending
            // just as often (the shared pool's change outputs), and the
            // deadline recomputes off valid_until so the window shrinks by
            // whatever the pay leg already used.
            let fee_picks = self
                .cache
                .select_for_disclosure_until(
                    &fee_key,
                    target,
                    slots,
                    id == "Amulet",
                    self.spend_reserve,
                    selection_deadline(&self.cache, &fee_key, target, valid_until_micros).await,
                    crate::rfq_v2::CONFIRM_SPLIT_POLL,
                )
                .await;
            let Some(fee_picks) = fee_picks else {
                return Ok(SwapOutcome::Requote {
                    reason: format!(
                        "{id} fee-funding selection failed (need {target} {id} for the settlement fee)"
                    ),
                });
            };
            picks.extend(fee_picks);
        }
        let own_cids: Vec<String> = picks.iter().map(|h| h.contract_id.clone()).collect();

        // Fresh timestamp: now_micros is from the pre-check, and the bounded
        // selection re-poll may have slept since — anchoring on it would
        // extend the reservation past valid_until + grace by the waited time.
        let remaining_micros =
            (valid_until_micros - chrono::Utc::now().timestamp_micros()).max(0) as u64;
        let expires_at =
            Instant::now() + Duration::from_micros(remaining_micros) + RESERVATION_GRACE;
        if !self
            .cache
            .reserve_v2(&own_cids, &env.quote.quote_id, expires_at)
            .await
        {
            return Ok(SwapOutcome::Requote {
                reason: "own holdings reservation raced".to_string(),
            });
        }

        // ---- PREPARE params straight from the envelope -------------------
        let params = AtomicDvpSettleParams {
            venue_cid: env.dvp.contract_id.clone(),
            quote: envelope.quote.clone(),
            quote_signature_der_hex: envelope.quote_signature.clone(),
            canonical_message: envelope.canonical_message.clone(),
            lp_party: lp_party.clone(),
            pair_name: venue("/pairName")?,
            base_instrument_id: venue("/baseInstrumentId/id")?,
            base_instrument_admin: venue("/baseInstrumentId/admin")?,
            quote_instrument_id: venue("/quoteInstrumentId/id")?,
            quote_instrument_admin: venue("/quoteInstrumentId/admin")?,
            quote_public_key_spki_hex: venue("/quotePublicKey")?,
            ticket_cid: env.ticket.as_ref().map(|t| t.contract_id.clone()),
            lp_input_holding_cids: envelope.lp_input_holding_cids.clone(),
            envelope_disclosures: envelope.disclosed.clone(),
            user_input_holding_cids: own_cids.clone(),
            synchronizer_id: self.config.synchronizer_id.clone(),
            lp_fees: signed_fees.clone(),
        };
        let expectation = OperationExpectation::AtomicDvpSettle {
            user_party: self.config.party_id.clone(),
            venue_cid: params.venue_cid.clone(),
            template_id: env.dvp.template_id.clone(),
            quote_id: env.quote.quote_id.clone(),
            ticket_id: env.quote.ticket_id.clone(),
            ticket_cid: params.ticket_cid.clone(),
            side: env.quote.side.clone(),
            base_amount: env.quote.base_amount.clone(),
            quote_amount: env.quote.quote_amount.clone(),
            lp_party,
            base_instrument_id: params.base_instrument_id.clone(),
            base_instrument_admin: params.base_instrument_admin.clone(),
            quote_instrument_id: params.quote_instrument_id.clone(),
            quote_instrument_admin: params.quote_instrument_admin.clone(),
            valid_until_micros,
            lp_input_holding_cids: params.lp_input_holding_cids.clone(),
            user_input_holding_cids: own_cids.clone(),
        };
        let req = PrepareAtomicTransactionRequest {
            params: Some(AtomicParams::AtomicDvpSettle(params)),
            request_signature: None,
        };

        if self.confirm && !self.dry_run {
            if let Err(e) = agent_logic::confirm::confirm_transaction(
                &self.confirm_lock,
                "Atomic DVP settle",
                &format!(
                    "quote {}: {} {} base / {} quote (LP {})",
                    env.quote.quote_id,
                    env.quote.side,
                    env.quote.base_amount,
                    env.quote.quote_amount,
                    accepted.lp_name
                ),
            )
            .await
            {
                self.cache.release_reservations(&own_cids).await;
                return Err(e);
            }
        }

        let filled_base = base_amount.try_into().unwrap_or(0.0_f64);

        // ---- VERIFY + SIGN + EXEC (with FAIL classification) -------------
        let mut atomic_client = match create_atomic_client(&self.config).await {
            Ok(c) => c,
            Err(e) => {
                self.cache.release_reservations(&own_cids).await;
                return Err(e);
            }
        };

        let mut prepare_retries = 0u32;
        loop {
            let result = atomic_client
                .submit_atomic_transaction(req.clone(), &expectation, self.verbose, self.dry_run, self.force)
                .await;

            match result {
                Ok(resp) if resp.success => {
                    // CONFIRM — process_tx_result semantics: consume own
                    // inputs (drops their reservations), adopt created holdings.
                    self.cache.mark_consumed(&own_cids, &resp.update_id).await;
                    self.adopt_created(&mut atomic_client, &resp.created_contracts_json)
                        .await;
                    return Ok(SwapOutcome::Filled(AtomicFill {
                        update_id: resp.update_id,
                        filled_base,
                    }));
                }
                Ok(_) => {
                    // Only the --dry-run path returns Ok with success=false.
                    self.cache.release_reservations(&own_cids).await;
                    return Ok(SwapOutcome::DryRun);
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let names_own_cid = own_cids.iter().any(|c| msg.contains(c.as_str()));

                    // RECONCILIATION GATE (design §7.1 FAIL): an ambiguous
                    // execute failure, or INACTIVE_CONTRACTS naming the user's
                    // OWN input cids, means the settle MAY HAVE LANDED. A
                    // same-envelope replay aborts on-ledger, but a re-quote
                    // after an unnoticed success is a SECOND real settle — the
                    // user fills (and pays) twice. Reconcile via ledger state
                    // BEFORE releasing or re-quoting; unknowable ⇒ Abort.
                    if is_ambiguous_execute_error(&e)
                        || (msg.contains("INACTIVE_CONTRACTS") && names_own_cid)
                    {
                        warn!(
                            "Ambiguous atomic execute for quote {} — reconciling own inputs before anything else: {}",
                            env.quote.quote_id, msg
                        );
                        return match self.poll_own_inputs(&own_cids).await {
                            ReconcileStatus::Consumed => {
                                info!(
                                    "Reconciliation: own inputs consumed — the settle for quote {} COMMITTED",
                                    env.quote.quote_id
                                );
                                self.cache
                                    .mark_consumed(&own_cids, "atomic-settle-reconciled")
                                    .await;
                                Ok(SwapOutcome::Filled(AtomicFill {
                                    update_id: "(reconciled; execute response lost)".to_string(),
                                    filled_base,
                                }))
                            }
                            ReconcileStatus::Live => {
                                self.cache.release_reservations(&own_cids).await;
                                Ok(SwapOutcome::Requote {
                                    reason: format!(
                                        "execute did not commit (own inputs verified live): {msg}"
                                    ),
                                })
                            }
                            ReconcileStatus::Unknown => Ok(SwapOutcome::Abort {
                                reason: format!(
                                    "cannot establish commit status of quote {} — check the ledger manually before re-quoting: {}",
                                    env.quote.quote_id, msg
                                ),
                            }),
                        };
                    }

                    // INACTIVE_CONTRACTS naming only LP cids: verify our own
                    // inputs are live, then it is safe to release + re-quote
                    // (the LP's ticket survives — only a real settle spends it).
                    if msg.contains("INACTIVE_CONTRACTS") {
                        return match self.poll_own_inputs(&own_cids).await {
                            ReconcileStatus::Live => {
                                self.cache.release_reservations(&own_cids).await;
                                Ok(SwapOutcome::Requote {
                                    reason: format!("LP inputs inactive (envelope stale): {msg}"),
                                })
                            }
                            ReconcileStatus::Consumed => {
                                self.cache
                                    .mark_consumed(&own_cids, "atomic-settle-reconciled")
                                    .await;
                                Ok(SwapOutcome::Filled(AtomicFill {
                                    update_id: "(reconciled; execute response lost)".to_string(),
                                    filled_base,
                                }))
                            }
                            ReconcileStatus::Unknown => Ok(SwapOutcome::Abort {
                                reason: format!(
                                    "INACTIVE_CONTRACTS with undeterminable own-input state: {msg}"
                                ),
                            }),
                        };
                    }

                    // Prepare-stage errors: nothing was submitted — retry
                    // within the signed validity window.
                    let now = chrono::Utc::now().timestamp_micros();
                    let window_open =
                        now + PRECHECK_VALIDITY_MARGIN_MICROS < valid_until_micros;
                    if msg.contains("PrepareAtomicTransaction")
                        && window_open
                        && prepare_retries < MAX_PREPARE_RETRIES
                    {
                        prepare_retries += 1;
                        warn!(
                            "Prepare failed (attempt {}/{}), retrying within the window: {}",
                            prepare_retries, MAX_PREPARE_RETRIES, msg
                        );
                        tokio::time::sleep(Duration::from_millis(2000)).await;
                        continue;
                    }

                    // Everything else (window abort, verification rejection,
                    // exhausted retries): nothing committed — release + re-quote.
                    self.cache.release_reservations(&own_cids).await;
                    return Ok(SwapOutcome::Requote { reason: msg });
                }
            }
        }
    }

    /// Poll the ledger for the user's own input cids: consumed ⇒ the settle
    /// landed; live ⇒ it did not; unknown ⇒ every poll failed (conservative).
    async fn poll_own_inputs(&self, own_cids: &[String]) -> ReconcileStatus {
        let mut client = match create_v1_client(&self.config).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Reconciliation: cannot create ledger client: {e:#}");
                return ReconcileStatus::Unknown;
            }
        };
        for attempt in 1..=RECONCILE_ATTEMPTS {
            let is_last = attempt == RECONCILE_ATTEMPTS;
            match client
                .get_active_contracts(&[TEMPLATE_AMULET.to_string(), TEMPLATE_HOLDING.to_string()])
                .await
            {
                Ok(contracts) => {
                    let live: HashSet<&str> =
                        contracts.iter().map(|c| c.contract_id.as_str()).collect();
                    if own_cids.iter().any(|c| !live.contains(c.as_str())) {
                        return ReconcileStatus::Consumed;
                    }
                    if is_last {
                        return ReconcileStatus::Live;
                    }
                }
                Err(e) => {
                    warn!(
                        "Reconciliation poll {}/{} failed: {e:#}",
                        attempt, RECONCILE_ATTEMPTS
                    );
                    if is_last {
                        return ReconcileStatus::Unknown;
                    }
                }
            }
            tokio::time::sleep(RECONCILE_POLL).await;
        }
        ReconcileStatus::Unknown
    }

    /// Adopt holdings created by our own settle into the cache. The execute
    /// response carries only `[{contract_id, template_id}]`, so amounts are
    /// backfilled via a targeted `GetAtomicContracts` fetch. Best-effort — the
    /// ACS worker reconciles within its 30 s cycle regardless.
    async fn adopt_created(&self, client: &mut AtomicProviderClient, created_json: &str) {
        let entries: Vec<Value> = match serde_json::from_str(created_json) {
            Ok(v) => v,
            Err(_) => return,
        };
        let cids: Vec<String> = entries
            .iter()
            .filter_map(|e| {
                let template = e.get("template_id")?.as_str()?;
                let holdingish = (template.contains("Amulet")
                    && !template.contains("Rules")
                    && !template.contains("Locked"))
                    || template.contains("Utility.Registry.Holding.V0.Holding:Holding");
                if !holdingish {
                    return None;
                }
                e.get("contract_id")?.as_str().map(str::to_string)
            })
            .collect();
        if cids.is_empty() {
            return;
        }

        let resp = match client.get_atomic_contracts(&[], &cids).await {
            Ok(r) => r,
            Err(e) => {
                warn!("Created-holdings backfill fetch failed (ACS refresh will catch up): {e:#}");
                return;
            }
        };
        let sync_by_cid: std::collections::HashMap<&str, &str> = resp
            .disclosures
            .iter()
            .map(|d| (d.contract_id.as_str(), d.synchronizer_id.as_str()))
            .collect();

        let now = Instant::now();
        let mut adopted: Vec<CachedHolding> = Vec::new();
        for c in &resp.contracts {
            let Ok(payload) = serde_json::from_str::<Value>(&c.payload_json) else {
                continue;
            };
            let synchronizer_id = sync_by_cid
                .get(c.contract_id.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| self.config.synchronizer_id.clone());
            let blob = (!c.created_event_blob.is_empty()).then(|| c.created_event_blob.clone());

            if c.template_id.contains("Splice.Amulet:Amulet") && !c.template_id.contains("Locked") {
                let Some(amount) = payload
                    .pointer("/amount/initialAmount")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Decimal>().ok())
                else {
                    continue;
                };
                adopted.push(CachedHolding {
                    contract_id: c.contract_id.clone(),
                    template_id: c.template_id.clone(),
                    instrument: CC_INSTRUMENT.to_string(),
                    amount,
                    created_event_blob: blob,
                    synchronizer_id,
                    discovered_at: now,
                });
            } else if c
                .template_id
                .contains("Utility.Registry.Holding.V0.Holding:Holding")
            {
                let owner_ok =
                    payload.get("owner").and_then(|o| o.as_str()) == Some(self.config.party_id.as_str());
                let lock = payload.pointer("/lock");
                let unlocked = lock.is_none() || lock.is_some_and(|l| l.is_null());
                if !owner_ok || !unlocked {
                    continue;
                }
                let (Some(amount), Some(admin), Some(id)) = (
                    payload.get("amount").and_then(|v| v.as_str()).and_then(|s| s.parse::<Decimal>().ok()),
                    payload.pointer("/instrument/source").and_then(|v| v.as_str()),
                    payload.pointer("/instrument/id").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                adopted.push(CachedHolding {
                    contract_id: c.contract_id.clone(),
                    template_id: c.template_id.clone(),
                    instrument: instrument_key(admin, id),
                    amount,
                    created_event_blob: blob,
                    synchronizer_id,
                    discovered_at: now,
                });
            }
        }
        if !adopted.is_empty() {
            info!("Adopted {} settle-created holdings into the cache", adopted.len());
            self.cache.add_created(adopted).await;
        }
    }
}
