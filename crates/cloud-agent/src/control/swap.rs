//! The swap command: RFQ V2 quotes from the LPs its signer may use, filtered with exact
//! decimals and ranked, then one atomic settle bound to the command before its only signature.

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

use std::future::Future;
use std::time::Duration;

use agent_control::{
    Amount, Command, CommandError, CommandFailure, CommandOp, ControllerId, EvalContext,
    InstrumentId, InstrumentRef, PartyId, ProofEnvelope, Rules, SwapResult, Token, check_swap_lp,
    scope_of, settle_terms, verify_command,
};
use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::control_store::{CommandAttempt, CommandEnd, CommandStatus, SwapAttempt};
use anyhow::{Result, anyhow, bail};
use orderbook_proto::ledger::{ControllerCommand, ExecuteTransactionResponse};
use orderbook_proto::orderbook::Market;
use orderbook_proto::pricing::GetPriceResponse;
use orderbook_proto::rfqv2::{
    AcceptQuoteAtomicResponse, AtomicQuoteEnvelope, AtomicQuoteInfo,
    ExecuteAtomicTransactionResponse, RequestQuotesV2Response,
};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use tracing::{info, warn};
use tx_verifier::facts::TxFacts;

use super::{
    Executor, Handled, Pending, Ready, Submitted, alert_for, canton_coin_of, classify,
    locked_state, reconcile, record_of, refuse, settle, usage_entries, usage_totals, withhold,
};
use crate::atomic_swap::{
    AtomicSwapper, DirectedBefore, DirectedBind, DirectedOutcome, DirectedRound, DirectedSettle,
};
use crate::control::intent::{self, DirectedSignature, OwnRequest, Prepared, SignatureRefusal};
use crate::fill_loop::FillDirection;
use crate::holdings_cache::{HoldingsCache, TEMPLATE_AMULET, TEMPLATE_HOLDING};
use crate::ledger_client::{AtomicProviderClient, PreSignContext};

/// The LPs' answer wait the rpc applies to one request or accept (it clamps to 1-30 s).
const RFQ_WAIT_SECS: u32 = 15;
/// Bound on one RFQ, price or market call.
const RFQ_TIMEOUT: Duration = Duration::from_secs(45);
/// Bound on connecting a client.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on ensuring the receiver preapproval, its own transaction included.
const PREAPPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
/// Bound on reading the agent's own holdings.
const HOLDINGS_TIMEOUT: Duration = Duration::from_secs(180);
/// Bound on one directed settle, its signed window included.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(3_900);

/// How the rounds of one swap are paced.
#[derive(Clone, Copy, Debug)]
pub(super) struct Pace {
    /// Least time between two quote requests (10 a minute).
    pub gap: Duration,
    /// Rounds stop this long before the command expires, or the longest signed window seen.
    pub tail_secs: i64,
    pub max_accepts: u32,
    pub max_rounds: u32,
}

pub(super) const PACE: Pace = Pace {
    gap: Duration::from_secs(10),
    tail_secs: 120,
    max_accepts: 5,
    max_rounds: 120,
};

/// The signed terms of a swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SwapOrder {
    pub sell: InstrumentRef,
    pub sell_amount: Amount,
    pub buy: InstrumentRef,
    pub min_receive: Amount,
    pub max_fee_cc: Amount,
}

impl SwapOrder {
    pub(super) fn of(op: &CommandOp) -> Option<Self> {
        match op {
            CommandOp::Swap {
                sell,
                sell_amount,
                buy,
                min_receive,
                max_fee_cc,
            } => Some(Self {
                sell: sell.clone(),
                sell_amount: *sell_amount,
                buy: buy.clone(),
                min_receive: *min_receive,
                max_fee_cc: *max_fee_cc,
            }),
            _ => None,
        }
    }
}

/// An active market, its legs as on-chain instruments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedMarket {
    pub market_id: String,
    pub base: InstrumentRef,
    pub quote: InstrumentRef,
    /// The rpc's ids of the legs.
    pub base_symbol: String,
    pub quote_symbol: String,
}

/// Where a swap trades, and which way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapPlan {
    pub market_id: String,
    /// The sold instrument is the base: a `sell` by `quantity`; otherwise a `buy` by `quote_quantity`.
    pub sells_base: bool,
    /// The rpc's id of the bought instrument.
    pub buy_symbol: String,
}

/// The active market whose legs are exactly {sell, buy}; the first by id when several are.
pub(super) fn plan_for(markets: &[ListedMarket], order: &SwapOrder) -> Option<SwapPlan> {
    let mut plans: Vec<SwapPlan> = markets
        .iter()
        .filter_map(|m| {
            if m.base == order.sell && m.quote == order.buy {
                Some(SwapPlan {
                    market_id: m.market_id.clone(),
                    sells_base: true,
                    buy_symbol: m.quote_symbol.clone(),
                })
            } else if m.quote == order.sell && m.base == order.buy {
                Some(SwapPlan {
                    market_id: m.market_id.clone(),
                    sells_base: false,
                    buy_symbol: m.base_symbol.clone(),
                })
            } else {
                None
            }
        })
        .collect();
    plans.sort_by(|a, b| a.market_id.cmp(&b.market_id));
    plans.into_iter().next()
}

/// One RFQ V2 request: the sold amount as canonical decimal text, never a float.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteRequest {
    pub market_id: String,
    pub direction: &'static str,
    pub quantity: String,
    pub quote_quantity: Option<String>,
    /// Empty asks every LP.
    pub lp_names: Vec<String>,
}

pub(super) fn request_of(plan: &SwapPlan, order: &SwapOrder, lp_names: &[String]) -> QuoteRequest {
    let amount = order.sell_amount.to_string();
    let (direction, quantity, quote_quantity) = if plan.sells_base {
        ("sell", amount, None)
    } else {
        ("buy", String::new(), Some(amount))
    };
    QuoteRequest {
        market_id: plan.market_id.clone(),
        direction,
        quantity,
        quote_quantity,
        lp_names: lp_names.to_vec(),
    }
}

/// The agent's reference mid for a market (quote per base) and the unix second its source read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference {
    pub mid: Decimal,
    pub at: i64,
}

/// The mid of a price answer: halfway between bid and ask when both are quoted, else the last
/// trade; none without a positive price or a timestamp.
pub(super) fn reference_of(price: &GetPriceResponse) -> Option<Reference> {
    let positive = |v: f64| {
        (v.is_finite() && v > 0.0)
            .then(|| Decimal::from_f64(v))
            .flatten()
    };
    let mid = match (price.bid.and_then(positive), price.ask.and_then(positive)) {
        (Some(bid), Some(ask)) => bid.checked_add(ask)?.checked_div(Decimal::TWO)?,
        _ => positive(price.last)?,
    };
    let at = price.timestamp.as_ref()?.seconds;
    (mid > Decimal::ZERO).then_some(Reference { mid, at })
}

/// Why a quote or an accepted round did not settle; the furthest names the failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Miss {
    rank: u8,
    pub failure: CommandFailure,
}

impl Miss {
    pub const OTHER_LP: Self = Self {
        rank: 0,
        failure: CommandFailure::NoAcceptableQuote,
    };
    /// A quote id no receipt can carry.
    pub const QUOTE_ID: Self = Self {
        rank: 0,
        failure: CommandFailure::NoAcceptableQuote,
    };
    pub const NO_REFERENCE: Self = Self {
        rank: 1,
        failure: CommandFailure::NoReferencePrice,
    };
    pub const DEVIATION: Self = Self {
        rank: 2,
        failure: CommandFailure::PriceDeviation,
    };
    pub const SIZE: Self = Self {
        rank: 3,
        failure: CommandFailure::InexactSize,
    };
    pub const FEE: Self = Self {
        rank: 4,
        failure: CommandFailure::FeeAboveCap,
    };
    pub const MINIMUM: Self = Self {
        rank: 5,
        failure: CommandFailure::BelowMinimum,
    };
    pub const REJECTED: Self = Self {
        rank: 6,
        failure: CommandFailure::LpRejected,
    };
    pub const WINDOW: Self = Self {
        rank: 7,
        failure: CommandFailure::WindowClosed,
    };
    pub const HOLDINGS: Self = Self {
        rank: 8,
        failure: CommandFailure::InsufficientHoldings,
    };
    pub const SUBMIT: Self = Self {
        rank: 9,
        failure: CommandFailure::SubmitFailed,
    };
    pub const SIGNING_REFERENCE: Self = Self {
        rank: 10,
        failure: CommandFailure::NoReferencePrice,
    };
    pub const SIGNING_DEVIATION: Self = Self {
        rank: 11,
        failure: CommandFailure::PriceDeviation,
    };
}

/// The miss of a round that ended before its submit, from its reason.
fn miss_of(reason: &str) -> Miss {
    if reason.contains("selection failed") {
        Miss::HOLDINGS
    } else if reason.contains("signed window ends") || reason.contains("validity ends") {
        Miss::WINDOW
    } else {
        Miss::REJECTED
    }
}

/// The local price guard and what it compares against.
#[derive(Clone, Copy, Debug)]
pub(super) struct PriceGuard<'a> {
    pub order: &'a SwapOrder,
    pub sells_base: bool,
    pub cc: &'a InstrumentRef,
    /// 0 turns the guard off.
    pub max_bps: u32,
    /// A reference older than this, in seconds, is stale.
    pub max_age_secs: i64,
    pub reference: Option<Reference>,
}

impl PriceGuard<'_> {
    /// The effective price, quote per base, the CC fee counted: what the agent gets for what it
    /// gives. A missing or stale reference is 1410; a worse price by more than the bound, 1411.
    pub(super) fn check(
        &self,
        now: i64,
        sold: Decimal,
        received: Decimal,
        fee: Decimal,
    ) -> Result<(), CommandFailure> {
        if self.max_bps == 0 {
            return Ok(());
        }
        let fresh = |r: &Reference| {
            now.checked_sub(r.at)
                .and_then(i64::checked_abs)
                .is_some_and(|age| age <= self.max_age_secs)
        };
        let Some(reference) = self.reference.filter(|r| r.mid > Decimal::ZERO && fresh(r)) else {
            return Err(CommandFailure::NoReferencePrice);
        };
        let net = if self.order.buy == *self.cc {
            received.checked_sub(fee)
        } else {
            Some(received)
        };
        let cost = if self.order.sell == *self.cc {
            sold.checked_add(fee)
        } else {
            Some(sold)
        };
        let price = match (net, cost) {
            (Some(net), Some(cost)) if net > Decimal::ZERO && cost > Decimal::ZERO => {
                if self.sells_base {
                    net.checked_div(cost)
                } else {
                    cost.checked_div(net)
                }
            }
            _ => None,
        };
        let worse = price.and_then(|p| {
            if self.sells_base {
                reference.mid.checked_sub(p)
            } else {
                p.checked_sub(reference.mid)
            }
        });
        let bps = worse
            .and_then(|w| w.checked_mul(Decimal::from(10_000)))
            .and_then(|w| w.checked_div(reference.mid));
        match bps {
            Some(bps) if bps <= Decimal::from(self.max_bps) => Ok(()),
            _ => Err(CommandFailure::PriceDeviation),
        }
    }
}

/// A quote that passed the filter, with what it pays.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Pick {
    pub quote: AtomicQuoteInfo,
    /// What the agent keeps of the bought instrument.
    pub net: Decimal,
    pub fee: Decimal,
    /// The bought amount before any fee.
    pub received: Decimal,
}

fn exact(text: &str) -> Option<Decimal> {
    Decimal::from_str_exact(text).ok()
}

/// The quote filter, in exact decimals: an allowed LP, the price guard first, the exact sold
/// leg, no fee or a capped CC one to the fee party, and at least the minimum, net.
pub(super) struct Filter<'a> {
    pub guard: PriceGuard<'a>,
    pub fee_party: Option<&'a PartyId>,
    pub now: i64,
    pub lp_allowed: &'a (dyn Fn(&PartyId) -> bool + Send + Sync),
}

impl Filter<'_> {
    pub(super) fn judge(&self, quote: &AtomicQuoteInfo) -> Result<Pick, Miss> {
        let order = self.guard.order;
        // The receipt of an executed swap carries the quote id as a token.
        Token::parse(&quote.quote_id).map_err(|_| Miss::QUOTE_ID)?;
        let lp = PartyId::parse(&quote.lp_party_id).map_err(|_| Miss::OTHER_LP)?;
        if !(self.lp_allowed)(&lp) {
            return Err(Miss::OTHER_LP);
        }
        let (Some(base), Some(quote_amount)) =
            (exact(&quote.quantity), exact(&quote.quote_quantity))
        else {
            return Err(Miss::SIZE);
        };
        let (sold, received) = if self.guard.sells_base {
            (base, quote_amount)
        } else {
            (quote_amount, base)
        };
        if sold <= Decimal::ZERO || received <= Decimal::ZERO {
            return Err(Miss::SIZE);
        }
        let fee = match &quote.settlement_fee {
            None => Decimal::ZERO,
            Some(f) => {
                let to_fee_party = self.fee_party.is_some_and(|p| p.as_str() == f.receiver);
                let in_cc = f.instrument_admin == self.guard.cc.admin.as_str()
                    && f.instrument_id == self.guard.cc.id.as_str();
                match exact(&f.amount) {
                    Some(amount) if to_fee_party && in_cc && amount >= Decimal::ZERO => amount,
                    _ => return Err(Miss::FEE),
                }
            }
        };
        self.guard
            .check(self.now, sold, received, fee)
            .map_err(|failure| match failure {
                CommandFailure::NoReferencePrice => Miss::NO_REFERENCE,
                _ => Miss::DEVIATION,
            })?;
        if sold != order.sell_amount.as_decimal() {
            return Err(Miss::SIZE);
        }
        if fee > order.max_fee_cc.as_decimal() {
            return Err(Miss::FEE);
        }
        let net = if order.buy == *self.guard.cc {
            received.checked_sub(fee).ok_or(Miss::MINIMUM)?
        } else {
            received
        };
        if net < order.min_receive.as_decimal() {
            return Err(Miss::MINIMUM);
        }
        Ok(Pick {
            quote: quote.clone(),
            net,
            fee,
            received,
        })
    }
}

/// Best net first, then the smaller fee, then the quote id.
pub(super) fn rank(picks: &mut [Pick]) {
    picks.sort_by(|a, b| {
        b.net
            .cmp(&a.net)
            .then(a.fee.cmp(&b.fee))
            .then_with(|| a.quote.quote_id.cmp(&b.quote.quote_id))
    });
}

/// What the rounds of one swap have seen.
#[derive(Debug, Default)]
struct Seen {
    furthest: Option<Miss>,
    accepts: u32,
    rounds: u32,
    /// The longest signed window of an accepted quote, in seconds.
    longest_window: i64,
}

impl Seen {
    fn miss(&mut self, miss: Miss) {
        if self.furthest.is_none_or(|f| miss.rank > f.rank) {
            self.furthest = Some(miss);
        }
    }

    fn failure(&self) -> CommandFailure {
        self.furthest
            .map_or(CommandFailure::NoAcceptableQuote, |m| m.failure)
    }
}

/// The RFQ venue and the atomic ledger a swap uses.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait SwapDesk: Send {
    /// The active markets, their legs resolved through the rpc's instruments.
    async fn markets(&mut self) -> Result<Vec<ListedMarket>>;
    /// The agent's reference mid for a market, from the price feed its strategies read.
    async fn reference(&mut self, market_id: &str) -> Result<Option<Reference>>;
    /// The names of the connected LPs among `parties`.
    async fn lp_names(&mut self, parties: &[PartyId]) -> Result<Vec<String>>;
    /// Ensures the agent may receive the bought instrument in one step.
    async fn ensure_preapproval(&mut self, plan: &SwapPlan) -> Result<()>;
    async fn request_quotes(&mut self, request: &QuoteRequest) -> Result<RequestQuotesV2Response>;
    async fn accept(&mut self, rfq_id: &str, quote_id: &str) -> Result<AcceptQuoteAtomicResponse>;
    /// The one directed settle of an accepted envelope.
    #[allow(clippy::too_many_arguments)]
    async fn settle(
        &mut self,
        envelope: &AtomicQuoteEnvelope,
        accepted: &AtomicQuoteInfo,
        direction: FillDirection,
        directed: &DirectedSettle<'_>,
        before_submit: DirectedBefore<'_>,
        bind: DirectedBind<'_>,
    ) -> Result<DirectedOutcome>;
    /// Drops the ledger client, so the next settle mints a new token.
    fn remint(&mut self);
}

/// `work` within `limit`.
async fn bounded<T>(
    limit: Duration,
    what: &str,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(limit, work)
        .await
        .map_err(|_| anyhow!("{what} did not answer within {limit:?}"))?
}

/// The rpc's venue and the agent's ledger, connected on first use.
pub struct LiveDesk {
    config: BaseConfig,
    /// The config with the rpc's instruments, once read.
    loaded: Option<BaseConfig>,
    rfq: Option<OrderbookClient>,
    atomic: Option<AtomicProviderClient>,
}

impl LiveDesk {
    pub fn new(config: BaseConfig) -> Self {
        Self {
            config,
            loaded: None,
            rfq: None,
            atomic: None,
        }
    }

    async fn loaded(&mut self) -> Result<BaseConfig> {
        if let Some(config) = &self.loaded {
            return Ok(config.clone());
        }
        let mut config = self.config.clone();
        bounded(
            RFQ_TIMEOUT,
            "the instrument registry",
            crate::populate_instruments(&mut config),
        )
        .await?;
        self.loaded = Some(config.clone());
        Ok(config)
    }

    async fn rfq(&mut self) -> Result<&mut OrderbookClient> {
        if self.rfq.is_none() {
            let client = bounded(
                CONNECT_TIMEOUT,
                "the orderbook rpc",
                OrderbookClient::new(&self.config),
            )
            .await?;
            self.rfq = Some(client);
        }
        self.rfq
            .as_mut()
            .ok_or_else(|| anyhow!("the orderbook client is not connected"))
    }

    async fn atomic(&mut self, config: &BaseConfig) -> Result<&mut AtomicProviderClient> {
        if self.atomic.is_none() {
            let client = bounded(
                CONNECT_TIMEOUT,
                "the ledger-service",
                crate::atomic_swap::create_atomic_client(config),
            )
            .await?;
            self.atomic = Some(client);
        }
        self.atomic
            .as_mut()
            .ok_or_else(|| anyhow!("the atomic client is not connected"))
    }
}

/// A market whose legs both resolve to instruments control can name.
fn listed(config: &BaseConfig, market: &Market) -> Option<ListedMarket> {
    let leg = |symbol: &str| -> Option<InstrumentRef> {
        let (id, admin) = config.try_resolve_instrument(symbol).ok()?;
        Some(InstrumentRef {
            admin: PartyId::parse(&admin).ok()?,
            id: InstrumentId::parse(&id).ok()?,
        })
    };
    Some(ListedMarket {
        market_id: market.market_id.clone(),
        base: leg(&market.base_instrument)?,
        quote: leg(&market.quote_instrument)?,
        base_symbol: market.base_instrument.clone(),
        quote_symbol: market.quote_instrument.clone(),
    })
}

/// The agent's own unlocked holdings, read once for one settle.
async fn own_holdings(config: &BaseConfig) -> Result<std::sync::Arc<HoldingsCache>> {
    let mut client = bounded(
        CONNECT_TIMEOUT,
        "the ledger-service",
        crate::atomic_swap::create_v1_client(config),
    )
    .await?;
    let started = std::time::Instant::now();
    let templates = [TEMPLATE_AMULET.to_string(), TEMPLATE_HOLDING.to_string()];
    let (contracts, complete) = bounded(
        HOLDINGS_TIMEOUT,
        "the holdings snapshot",
        client.get_active_contracts_partial(&templates),
    )
    .await?;
    let holdings = crate::acs_worker::parse_acs_holdings(contracts, &config.party_id);
    let cache = HoldingsCache::new(false);
    if complete {
        cache.refresh_from_acs_snapshot(holdings, started).await;
    } else {
        cache.add_created(holdings).await;
    }
    Ok(cache)
}

#[async_trait::async_trait]
impl SwapDesk for LiveDesk {
    async fn markets(&mut self) -> Result<Vec<ListedMarket>> {
        let config = self.loaded().await?;
        let markets = bounded(RFQ_TIMEOUT, "GetMarkets", self.rfq().await?.get_markets()).await?;
        Ok(markets
            .iter()
            .filter(|m| m.is_active)
            .filter_map(|m| listed(&config, m))
            .collect())
    }

    async fn reference(&mut self, market_id: &str) -> Result<Option<Reference>> {
        let price = bounded(
            RFQ_TIMEOUT,
            "GetPrice",
            self.rfq().await?.get_price(market_id),
        )
        .await?;
        Ok(reference_of(&price))
    }

    async fn lp_names(&mut self, parties: &[PartyId]) -> Result<Vec<String>> {
        let lps = bounded(
            RFQ_TIMEOUT,
            "GetAtomicLiquidityProviders",
            self.rfq().await?.get_atomic_liquidity_providers(),
        )
        .await?;
        let mut names: Vec<String> = lps
            .into_iter()
            .filter(|lp| {
                !lp.lp_name.is_empty() && parties.iter().any(|p| p.as_str() == lp.party_id)
            })
            .map(|lp| lp.lp_name)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    async fn ensure_preapproval(&mut self, plan: &SwapPlan) -> Result<()> {
        let config = self.loaded().await?;
        let mut client = bounded(
            CONNECT_TIMEOUT,
            "the ledger-service",
            crate::atomic_swap::create_v1_client(&config),
        )
        .await?;
        let ensured = crate::atomic_swap::ensure_receiver_preapproval(
            &config,
            &mut client,
            &plan.buy_symbol,
            false,
            false,
            false,
        );
        bounded(PREAPPROVAL_TIMEOUT, "the receiver preapproval", ensured)
            .await
            .map(|_| ())
    }

    async fn request_quotes(&mut self, request: &QuoteRequest) -> Result<RequestQuotesV2Response> {
        let rfq = self.rfq().await?;
        let call = rfq.request_quotes_atomic(
            &request.market_id,
            request.direction,
            &request.quantity,
            request.quote_quantity.as_deref(),
            request.lp_names.clone(),
            Some(RFQ_WAIT_SECS),
            Vec::new(),
        );
        bounded(RFQ_TIMEOUT, "RequestQuotes", call).await
    }

    async fn accept(&mut self, rfq_id: &str, quote_id: &str) -> Result<AcceptQuoteAtomicResponse> {
        let rfq = self.rfq().await?;
        bounded(
            RFQ_TIMEOUT,
            "AcceptQuoteAtomic",
            rfq.accept_quote_atomic(rfq_id, quote_id, Some(RFQ_WAIT_SECS)),
        )
        .await
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
        let config = self.loaded().await?;
        let cache = own_holdings(&config).await?;
        let swapper = AtomicSwapper {
            config: config.clone(),
            cache,
            spend_reserve: true,
            verbose: false,
            dry_run: false,
            force: false,
            confirm: false,
            confirm_lock: agent_logic::confirm::new_confirm_lock(),
        };
        let client = self.atomic(&config).await?;
        let settled = swapper.settle_envelope_directed(
            envelope,
            accepted,
            direction,
            directed,
            client,
            before_submit,
            bind,
        );
        bounded(SETTLE_TIMEOUT, "the directed settle", settled).await
    }

    fn remint(&mut self) {
        self.atomic = None;
    }
}

/// The desk of the executor's ledger.
fn desk_of<'e>(exec: &'e mut Executor<'_>) -> Result<&'e mut dyn SwapDesk> {
    exec.ledger
        .swaps()
        .ok_or_else(|| anyhow!("this agent has no RFQ desk for swaps"))
}

/// The receipt result of an executed swap, from its recorded quote.
pub(super) fn result_of(swap: &SwapAttempt) -> Result<SwapResult> {
    let bad = |what: &str| anyhow!("the recorded swap has an invalid {what}");
    Ok(SwapResult {
        quote_id: Token::parse(&swap.quote_id).map_err(|_| bad("quote id"))?,
        lp: PartyId::parse(&swap.lp).map_err(|_| bad("LP"))?,
        sold: Amount::parse(&swap.sold).map_err(|_| bad("sold amount"))?,
        received: Amount::parse(&swap.received).map_err(|_| bad("received amount"))?,
        fee_cc: Amount::parse(&swap.fee_cc).map_err(|_| bad("fee"))?,
    })
}

/// Canonical decimal text, as receipts carry it.
fn canonical(d: Decimal) -> Result<String> {
    Amount::from_decimal(d)
        .map(|a| a.to_string())
        .map_err(|_| anyhow!("{d} is not an amount a receipt carries"))
}

/// What a whole swap is bound to.
struct Keys<'a> {
    pending: &'a Pending,
    command: &'a Command,
    order: &'a SwapOrder,
    envelope: &'a ProofEnvelope,
    signer: ControllerId,
    cc: &'a InstrumentRef,
    platform: &'a intent::PlatformParties,
    pinned: &'a agent_control::PlatformParties,
}

/// How one accepted round ended.
enum Settled {
    Done(Handled),
    Again(Miss),
}

/// The swap of a verified command: begun under the lock, then rounds of quotes until one
/// settles, the command is refused, or the rounds run out (failed with the furthest miss).
pub(super) async fn attempt(exec: &mut Executor<'_>, ready: Ready) -> Result<Handled> {
    run(exec, ready, PACE).await
}

pub(super) async fn run(exec: &mut Executor<'_>, ready: Ready, pace: Pace) -> Result<Handled> {
    let ctx = exec.ctx;
    let rpc = exec.rpc;
    let Ready {
        pending, command, ..
    } = ready;
    let id = pending.id.as_str().to_string();
    let order = SwapOrder::of(&command.op).ok_or_else(|| anyhow!("command {id} is not a swap"))?;
    let cc = canton_coin_of(&ctx.config)?;
    // The startup pins, and any the poller picked up from the env file since.
    let platform = ctx.platform();
    let pinned = platform.pinned().map_err(|e| anyhow!(e))?;
    let envelope = ProofEnvelope::decode(&pending.proof)
        .map_err(|e| anyhow!("the proof does not decode ({})", e.code_name()))?;
    let offset =
        match tokio::time::timeout(super::LEDGER_END_TIMEOUT, exec.ledger.ledger_end()).await {
            Ok(Ok(offset)) if offset > 0 => offset,
            Ok(Ok(_)) => return Ok(Handled::NotYet("the ledger end is unknown".into())),
            Ok(Err(e)) => {
                return Ok(Handled::NotYet(format!(
                    "the ledger end could not be read: {e:#}"
                )));
            }
            Err(_) => {
                return Ok(Handled::NotYet(format!(
                    "the ledger end was not read within {:?}",
                    super::LEDGER_END_TIMEOUT
                )));
            }
        };
    let rules = {
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
        state.rules_at(now).rules().clone()
    };
    let keys = Keys {
        pending: &pending,
        command: &command,
        order: &order,
        envelope: &envelope,
        signer: *envelope.controller_id(),
        cc: &cc,
        platform: &platform,
        pinned: &pinned,
    };
    let ran = rounds(exec, &keys, &rules, pace).await;
    let unsigned = matches!(ran, Err(_) | Ok(Handled::NotYet(_)));
    if unsigned {
        // Nothing was signed: the command is received again and may run later.
        let released = async {
            let lock = ctx.store.lock().await?;
            let at = agent_logic::clock::now_secs_i64();
            ctx.store
                .update_commands(&lock, at, |book| book.release(&id, at))
        }
        .await;
        if let Err(e) = released {
            warn!("agent control: swap {id} stopped and was not released: {e:#}");
        }
    }
    ran
}

/// The rounds: plan, preapproval, then quotes until one settles or the time runs out.
async fn rounds(
    exec: &mut Executor<'_>,
    keys: &Keys<'_>,
    rules: &Rules,
    pace: Pace,
) -> Result<Handled> {
    let ctx = exec.ctx;
    let rpc = exec.rpc;
    let id = keys.pending.id.as_str().to_string();
    let lps: &[PartyId] = scope_of(rules, &keys.signer).map_or(&[], |s| s.lps.as_slice());
    let markets = match desk_of(exec) {
        Ok(desk) => desk.markets().await,
        Err(e) => return Ok(Handled::NotYet(format!("{e:#}"))),
    };
    let markets = match markets {
        Ok(markets) => markets,
        Err(e) => {
            return Ok(Handled::NotYet(format!(
                "the markets could not be read: {e:#}"
            )));
        }
    };
    let Some(plan) = plan_for(&markets, keys.order) else {
        warn!(
            "agent control: swap {id}: no active market trades {} for {}",
            keys.order.sell.id, keys.order.buy.id
        );
        return fail(exec, keys.pending, CommandFailure::NoAcceptableQuote).await;
    };
    if let Err(e) = desk_of(exec)?.ensure_preapproval(&plan).await {
        warn!(
            "agent control: swap {id}: the receiver preapproval for {} is unavailable: {e:#}",
            plan.buy_symbol
        );
        return fail(exec, keys.pending, CommandFailure::PreapprovalUnavailable).await;
    }
    info!(
        "agent control: swap {id}: {} {} for at least {} {} on {} ({})",
        keys.order.sell_amount,
        keys.order.sell.id,
        keys.order.min_receive,
        keys.order.buy.id,
        plan.market_id,
        if plan.sells_base {
            "selling the base"
        } else {
            "buying the base by quote size"
        }
    );
    let max_bps = ctx.config.control.swap_max_deviation_bps;
    let max_age_secs = i64::from(ctx.config.control.swap_reference_max_age_secs);
    let op = &keys.command.op;
    // The binding checks the LP again at signing, against the rules in force then.
    let lp_allowed = |lp: &PartyId| -> bool {
        lp != &ctx.party
            && !keys.pinned.contains(lp)
            && check_swap_lp(rules, &keys.signer, op, lp).is_ok()
    };
    let expires_at = keys.command.expires_at.unix_secs();
    let mut seen = Seen::default();
    let mut last_request: Option<tokio::time::Instant> = None;
    loop {
        if seen.rounds >= pace.max_rounds || seen.accepts >= pace.max_accepts {
            break;
        }
        let now = rpc.checked_now()?.unix_secs();
        let stop_at = expires_at.saturating_sub(pace.tail_secs.max(seen.longest_window));
        if let Some(last) = last_request {
            let next = last.checked_add(pace.gap).unwrap_or(last);
            let wait = next.saturating_duration_since(tokio::time::Instant::now());
            let left = u64::try_from(stop_at.saturating_sub(now)).unwrap_or(0);
            if wait.as_secs() >= left {
                break;
            }
            tokio::time::sleep(wait).await;
        }
        let now = rpc.checked_now()?.unix_secs();
        if now >= stop_at {
            if seen.rounds == 0 {
                seen.miss(Miss::WINDOW);
            }
            break;
        }
        seen.rounds = seen.rounds.saturating_add(1);
        last_request = Some(tokio::time::Instant::now());
        let names = if lps.is_empty() {
            Vec::new()
        } else {
            match desk_of(exec)?.lp_names(lps).await {
                Ok(names) if !names.is_empty() => names,
                Ok(_) => {
                    info!(
                        "agent control: swap {id}: none of the LPs its scope allows is connected"
                    );
                    continue;
                }
                Err(e) => {
                    warn!("agent control: swap {id}: the LP list could not be read: {e:#}");
                    continue;
                }
            }
        };
        let reference = if max_bps > 0 {
            read_reference(exec, &plan.market_id).await
        } else {
            None
        };
        let request = request_of(&plan, keys.order, &names);
        let response = match desk_of(exec)?.request_quotes(&request).await {
            Ok(response) => response,
            Err(e) => {
                warn!("agent control: swap {id}: the quote request failed: {e:#}");
                continue;
            }
        };
        // The quotes are judged at the clock they arrived by.
        let now = rpc.checked_now()?.unix_secs();
        let guard = PriceGuard {
            order: keys.order,
            sells_base: plan.sells_base,
            cc: keys.cc,
            max_bps,
            max_age_secs,
            reference,
        };
        let filter = Filter {
            guard,
            fee_party: keys.pinned.fee.as_ref(),
            now,
            lp_allowed: &lp_allowed,
        };
        let mut picks = Vec::new();
        for quote in &response.quotes {
            match filter.judge(quote) {
                Ok(pick) => picks.push(pick),
                Err(miss) => {
                    info!(
                        "agent control: swap {id}: quote {} of {} not taken: {}",
                        quote.quote_id,
                        quote.lp_name,
                        miss.failure.code_name()
                    );
                    seen.miss(miss);
                }
            }
        }
        rank(&mut picks);
        let Some(best) = picks.into_iter().next() else {
            continue;
        };
        seen.accepts = seen.accepts.saturating_add(1);
        let accepted = match desk_of(exec)?
            .accept(&response.rfq_id, &best.quote.quote_id)
            .await
        {
            Ok(accepted) if accepted.success => accepted,
            Ok(accepted) => {
                warn!(
                    "agent control: swap {id}: LP {} refused the accept of quote {}: {}",
                    best.quote.lp_name,
                    best.quote.quote_id,
                    agent_logic::client::atomic_reject_reason_name(accepted.reject_reason)
                );
                seen.miss(Miss::REJECTED);
                continue;
            }
            Err(e) => {
                warn!(
                    "agent control: swap {id}: the accept of quote {} failed: {e:#}",
                    best.quote.quote_id
                );
                seen.miss(Miss::REJECTED);
                continue;
            }
        };
        let Some(envelope) = accepted.envelope else {
            seen.miss(Miss::REJECTED);
            continue;
        };
        if let Some(quote) = envelope.quote.as_ref() {
            let window = quote
                .valid_until_micros
                .div_euclid(1_000_000)
                .saturating_sub(now);
            seen.longest_window = seen.longest_window.max(window);
        }
        match settle_one(exec, keys, &plan, &envelope, &best, max_bps).await? {
            Settled::Done(handled) => return Ok(handled),
            Settled::Again(miss) => {
                info!(
                    "agent control: swap {id}: quote {} not settled: {}; nothing was signed",
                    best.quote.quote_id,
                    miss.failure.code_name()
                );
                seen.miss(miss);
            }
        }
    }
    fail(exec, keys.pending, seen.failure()).await
}

/// The reference for the price guard; a failure to read it counts as none.
async fn read_reference(exec: &mut Executor<'_>, market_id: &str) -> Option<Reference> {
    let desk = desk_of(exec).ok()?;
    match desk.reference(market_id).await {
        Ok(reference) => reference,
        Err(e) => {
            warn!("agent control: the reference price of {market_id} could not be read: {e:#}");
            None
        }
    }
}

/// The settle of one accepted envelope under the lock: the binding, scope, policy, packages
/// and price guard are checked on the hashed bytes right before the only signature.
async fn settle_one(
    exec: &mut Executor<'_>,
    keys: &Keys<'_>,
    plan: &SwapPlan,
    accepted: &AtomicQuoteEnvelope,
    pick: &Pick,
    max_bps: u32,
) -> Result<Settled> {
    let ctx = exec.ctx;
    let rpc = exec.rpc;
    let pending = keys.pending;
    let id = pending.id.as_str().to_string();
    let reference = if max_bps > 0 {
        read_reference(exec, &plan.market_id).await
    } else {
        None
    };
    let (lock, state) = locked_state(ctx).await?;
    match record_of(&ctx.store, &lock, &id)? {
        Some(record) if record.status == CommandStatus::Executing && !record.may_be_signed() => {}
        Some(record) => bail!(
            "swap {id} is {:?} in the local book; this run stops",
            record.status
        ),
        None => bail!("swap {id} is not recorded"),
    }
    let now = rpc.checked_now()?;
    if let Err(e) = verify_command(&state, &pending.bytes, keys.envelope, now, Some(keys.cc)) {
        return refuse(
            exec.answers(),
            lock,
            &state,
            pending,
            e.code(),
            e.code_name(),
        )
        .await
        .map(Settled::Done);
    }
    let usage = match usage_totals(&ctx.store, now).await {
        Ok(usage) => usage,
        Err(e) => {
            return Ok(Settled::Done(Handled::NotYet(format!(
                "the spend counters could not be read: {e:#}"
            ))));
        }
    };
    let direction = if plan.sells_base {
        FillDirection::Sell
    } else {
        FillDirection::Buy
    };
    let directed = DirectedSettle {
        command: keys.command,
        controller_command: ControllerCommand {
            canonical: pending.bytes.clone(),
            controller_id: keys.envelope.controller_id().to_string(),
            proof: pending.proof.clone(),
        },
        expires_at_micros: keys
            .command
            .expires_at
            .unix_secs()
            .saturating_mul(1_000_000),
    };
    let mut refusal: Option<SignatureRefusal> = None;
    let mut price_miss: Option<Miss> = None;
    let outcome = {
        let (state, lock, id) = (&state, &lock, id.as_str());
        let agent = ctx.config.party_id.as_str();
        let guard = PriceGuard {
            order: keys.order,
            sells_base: plan.sells_base,
            cc: keys.cc,
            max_bps,
            max_age_secs: i64::from(ctx.config.control.swap_reference_max_age_secs),
            reference,
        };
        let mut before = |round: &DirectedRound<'_>| -> Result<()> {
            let swap = SwapAttempt {
                quote_id: round.quote_id.to_string(),
                lp: pick.quote.lp_party_id.clone(),
                sold: keys.order.sell_amount.to_string(),
                received: canonical(pick.received)?,
                fee_cc: canonical(pick.fee)?,
                inputs: round.own_cids.to_vec(),
                valid_until: round.valid_until_micros.div_euclid(1_000_000),
            };
            // Nothing is signed whose executed receipt could not carry this record.
            result_of(&swap)?;
            let at = rpc.checked_now()?.unix_secs();
            ctx.store
                .update_commands(lock, at, |book| book.set_swap(id, swap, at))
        };
        let mut bind = |round: &DirectedRound<'_>, pc: &PreSignContext<'_>| -> Result<()> {
            let now = rpc.checked_now()?;
            let eval = EvalContext {
                agent_party: &ctx.party,
                cc: keys.cc,
                platform: keys.pinned,
                now,
                usage: &usage,
            };
            let signature = DirectedSignature {
                state,
                now,
                command: &pending.bytes,
                envelope: keys.envelope,
                ctx: &eval,
            };
            let prepared = Prepared {
                expectation: round.expectation,
                request: OwnRequest::Atomic(round.params),
                verification: pc.verification,
                facts: pc.facts,
                prepared_transaction: pc.prepared_transaction,
            };
            let authorization = match intent::check_directed_swap(
                agent,
                keys.platform,
                &prepared,
                &signature,
                round.venue_cid,
            ) {
                Ok(authorization) => authorization,
                Err(refused) => {
                    let text = refused.to_string();
                    refusal = Some(refused);
                    bail!("{text}")
                }
            };
            if let Err(failure) = guard_on_facts(&guard, pc.facts, &ctx.party, now.unix_secs()) {
                price_miss = Some(if failure == CommandFailure::NoReferencePrice {
                    Miss::SIGNING_REFERENCE
                } else {
                    Miss::SIGNING_DEVIATION
                });
                bail!(
                    "the settle fails the price guard at signing: {}",
                    failure.code_name()
                );
            }
            let uses = usage_entries(&authorization.decision.usage);
            let at = now.unix_secs();
            ctx.store
                .update_commands(lock, at, |book| book.mark_signing(id, uses, at))
        };
        desk_of(exec)?
            .settle(
                accepted,
                &pick.quote,
                direction,
                &directed,
                &mut before,
                &mut bind,
            )
            .await
    };
    let (signed, result) = match outcome {
        Ok(DirectedOutcome::NotSubmitted(reason)) => {
            drop(lock);
            info!(
                "agent control: swap {id}: the round of quote {} ended unsubmitted: {reason}",
                pick.quote.quote_id
            );
            return Ok(Settled::Again(miss_of(&reason)));
        }
        Ok(DirectedOutcome::Submitted { signed, result }) => (signed, result),
        Err(e) => {
            // Nothing passed the hook unless the book says so.
            let signing = record_of(&ctx.store, &lock, &id)?.is_some_and(|r| r.may_be_signed());
            if !signing {
                drop(lock);
                warn!(
                    "agent control: swap {id}: the settle of quote {} stopped unsigned: {e:#}",
                    pick.quote.quote_id
                );
                return Ok(Settled::Again(Miss::SUBMIT));
            }
            (true, Ok(Err(e)))
        }
    };
    if let (false, Some(miss)) = (signed, price_miss) {
        drop(lock);
        return Ok(Settled::Again(miss));
    }
    let result = result.map(|r| r.map(as_v1_response));
    let refusal_text = refusal.as_ref().map(ToString::to_string);
    let submitted = classify(result, signed, refusal);
    alert_for(ctx, &id, &submitted, refusal_text.as_deref());
    match submitted {
        Submitted::Executed(update_id) => {
            info!(
                "agent control: swap {id} executed: update {update_id} (quote {})",
                pick.quote.quote_id
            );
            let withheld = withhold(exec.answers(), &lock, &pending.id, &update_id).await?;
            if let Some(withheld) = withheld {
                drop(lock);
                return Ok(Settled::Done(withheld));
            }
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
            Ok(Settled::Done(Handled::Executed {
                update_id,
                delivered,
            }))
        }
        Submitted::Answered {
            refused,
            code,
            name,
            from_ledger,
        } => {
            warn!(
                "agent control: swap {id} {} before its signature: {name} (code {code})",
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
            Ok(Settled::Done(if refused {
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
            }))
        }
        Submitted::Again(why) => {
            drop(lock);
            info!(
                "agent control: swap {id}: quote {} not signed: {why}",
                pick.quote.quote_id
            );
            // The control log or the token is repaired first; the swap then runs again.
            if matches!(
                super::refusal_code(&why),
                Some(super::LOG_BEHIND | super::TOKEN_REFUSED)
            ) {
                return Ok(Settled::Done(Handled::NotYet(why)));
            }
            Ok(Settled::Again(Miss::SUBMIT))
        }
        Submitted::Open { verdict, why } => {
            warn!(
                "agent control: swap {id} was signed and its outcome is not known yet ({why}); it is reconciled, never signed again"
            );
            let at = rpc.checked_now().map_or(now.unix_secs(), |t| t.unix_secs());
            if let Some(verdict) = verdict {
                ctx.store
                    .update_commands(&lock, at, |book| book.set_verdict(&id, verdict, at))?;
            }
            drop(lock);
            reconcile(exec, &id).await.map(Settled::Done)
        }
    }
}

/// The price guard again at signing, on the terms the bound transaction settles.
fn guard_on_facts(
    guard: &PriceGuard<'_>,
    facts: &Result<TxFacts, String>,
    agent: &PartyId,
    now: i64,
) -> Result<(), CommandFailure> {
    if guard.max_bps == 0 {
        return Ok(());
    }
    let facts = facts.as_ref().map_err(|_| CommandFailure::PriceDeviation)?;
    let terms = settle_terms(facts, agent).map_err(|_| CommandFailure::PriceDeviation)?;
    let fee = terms
        .fees
        .iter()
        .try_fold(Decimal::ZERO, |total, fee| {
            total.checked_add(fee.amount.as_decimal())
        })
        .ok_or(CommandFailure::PriceDeviation)?;
    guard.check(
        now,
        terms.sent.amount.as_decimal(),
        terms.received.amount.as_decimal(),
        fee,
    )
}

/// The atomic execute answer as the transfer classification reads it.
fn as_v1_response(r: ExecuteAtomicTransactionResponse) -> ExecuteTransactionResponse {
    ExecuteTransactionResponse {
        success: r.success,
        update_id: r.update_id,
        error_message: (!r.message.is_empty()).then_some(r.message),
        ..Default::default()
    }
}

/// Fails a swap that is still executing here, with a signed receipt.
async fn fail(
    exec: &mut Executor<'_>,
    pending: &Pending,
    failure: CommandFailure,
) -> Result<Handled> {
    let ctx = exec.ctx;
    let id = pending.id.as_str();
    let (lock, state) = locked_state(ctx).await?;
    match record_of(&ctx.store, &lock, id)? {
        Some(record) if record.status == CommandStatus::Executing && !record.may_be_signed() => {}
        Some(record) => {
            drop(lock);
            return Ok(Handled::Settled {
                status: record.status,
                delivered: record.receipt_posted,
            });
        }
        None => bail!("swap {id} is not recorded"),
    }
    let (code, name) = (failure.code(), failure.code_name());
    info!("agent control: swap {id} failed: {name} (code {code}); nothing was signed");
    let delivered = settle(
        exec.answers(),
        lock,
        &state,
        &pending.id,
        pending.hash,
        CommandEnd::Failed { code },
        false,
    )
    .await?;
    Ok(Handled::Failed {
        code,
        name,
        delivered,
    })
}

#[cfg(test)]
#[path = "swap/tests.rs"]
mod tests;
