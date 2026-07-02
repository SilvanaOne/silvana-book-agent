// Port of agent-concentration-risk enforcement logic to TypeScript. Mirrors
// crates/agent-concentration-risk/src/main.rs (fn sweep).
//
// Enforcement variant of agent-risk-exposure:
//   share > max_share_pct  → cancel own BIDs on that market (no further accumulation)
//   share < min_share_pct  → cancel own OFFERs on that market (no further depletion)
//   dry-run: log only, don't touch orders.

export type Instrument = {
  name: string;              // BASE instrument, e.g. "Amulet"
  market: string;            // e.g. "CC-USDC"
  balance: number;           // current unlocked balance in base units
  priceMultiplier: number;   // quote-value = balance × currentPrice × priceMultiplier
  bidsActive: number;        // number of own live BIDs on this market
  offersActive: number;      // number of own live OFFERs on this market
  sharePct?: number;         // populated by step()
  valueQuote?: number;       // populated by step()
};

export type ConcentrationRiskConfig = Readonly<{
  instruments: Instrument[];
  maxSharePct: number;         // cancel bids when share > this
  minSharePct: number;         // cancel offers when share < this
  checkIntervalSecs: number;   // sweep cadence
  dryRun: boolean;
  balanceDriftPerTick: number; // magnitude of random walk applied to balance per tick
  startingPrice: number;
}>;

export type Cancellation = Readonly<{
  seq: number;
  t: number;                 // epoch ms
  instrument: string;
  market: string;
  side: "BID" | "OFFER";
  ordersCancelled: number;
  reason: "over-max" | "under-min";
  sharePct: number;
  thresholdPct: number;
  dryRun: boolean;
}>;

export type ConcentrationRiskState = {
  config: ConcentrationRiskConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  positions: Instrument[];   // live snapshot (with sharePct + valueQuote)
  totalPortfolio: number;    // sum of valueQuote across positions
  cancellationsHistory: Cancellation[];
  cancellationsCount: number;
  breachedInstrument: string | null;
  breachedKind: "over" | "under" | null;
  checksCount: number;
  lastCheckAt?: number;
  samples: number;           // ticks observed
};

const MAX_CANCELS = 30;

export function initState(config: ConcentrationRiskConfig, startPrice: number): ConcentrationRiskState {
  // Deep clone instruments so we can mutate freely.
  const positions = config.instruments.map((i) => ({ ...i }));
  const state: ConcentrationRiskState = {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    positions,
    totalPortfolio: 0,
    cancellationsHistory: [],
    cancellationsCount: 0,
    breachedInstrument: null,
    breachedKind: null,
    checksCount: 0,
    samples: 0,
  };
  recomputeShares(state);
  return state;
}

function recomputeShares(state: ConcentrationRiskState): void {
  let total = 0;
  for (const p of state.positions) {
    const v = Math.max(0, p.balance) * state.currentPrice * p.priceMultiplier;
    p.valueQuote = v;
    total += v;
  }
  state.totalPortfolio = total;
  for (const p of state.positions) {
    p.sharePct = total > 0 ? ((p.valueQuote ?? 0) / total) * 100 : 0;
  }
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: ConcentrationRiskState, price: number, now: number): { state: ConcentrationRiskState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.samples += 1;

  // Random walk on balances so shares actually drift over time.
  const drift = state.config.balanceDriftPerTick;
  if (drift > 0) {
    for (const p of state.positions) {
      const jitter = (Math.random() - 0.5) * 2 * drift;   // uniform [-drift, +drift]
      // scale jitter by price multiplier so cross-instrument balances stay sensible
      const scaled = jitter / Math.max(1, Math.sqrt(p.priceMultiplier));
      p.balance = Math.max(0, p.balance + scaled);
    }
  }

  recomputeShares(state);

  // Periodic sweep — check every checkIntervalSecs (ticks are 1s).
  const interval = Math.max(1, Math.floor(state.config.checkIntervalSecs));
  if (state.samples % interval !== 0) return { state, events };

  state.checksCount += 1;
  state.lastCheckAt = now;
  const { maxSharePct, minSharePct, dryRun } = state.config;

  state.breachedInstrument = null;
  state.breachedKind = null;

  if (state.totalPortfolio <= 0) {
    events.push(`CHECK #${state.checksCount}: portfolio_value=0 — nothing to enforce`);
    return { state, events };
  }

  for (const p of state.positions) {
    const share = p.sharePct ?? 0;
    if (share > maxSharePct && p.bidsActive > 0) {
      const cancelled = p.bidsActive;
      recordCancel(state, now, p, "BID", cancelled, "over-max", share, maxSharePct, dryRun);
      if (!dryRun) p.bidsActive = 0;
      state.breachedInstrument = p.name;
      state.breachedKind = "over";
      events.push(
        `${dryRun ? "[dry-run] " : ""}CANCEL BIDs on ${p.market} — ${p.name} share ${share.toFixed(2)}% > ${maxSharePct}% (${cancelled} orders)`,
      );
    } else if (share < minSharePct && p.offersActive > 0) {
      const cancelled = p.offersActive;
      recordCancel(state, now, p, "OFFER", cancelled, "under-min", share, minSharePct, dryRun);
      if (!dryRun) p.offersActive = 0;
      state.breachedInstrument = p.name;
      state.breachedKind = "under";
      events.push(
        `${dryRun ? "[dry-run] " : ""}CANCEL OFFERs on ${p.market} — ${p.name} share ${share.toFixed(2)}% < ${minSharePct}% (${cancelled} orders)`,
      );
    } else if (share > maxSharePct || share < minSharePct) {
      events.push(
        `CHECK #${state.checksCount}: ${p.name}@${p.market} breach (share=${share.toFixed(2)}%) but no own ${share > maxSharePct ? "BID" : "OFFER"} orders to cancel`,
      );
    }
  }
  if (state.breachedInstrument === null && !events.length) {
    // Emit a low-frequency heartbeat only when nothing else happened.
    events.push(
      `CHECK #${state.checksCount}: all instruments within [${minSharePct}%, ${maxSharePct}%]`,
    );
  }
  return { state, events };
}

function recordCancel(
  state: ConcentrationRiskState,
  now: number,
  p: Instrument,
  side: "BID" | "OFFER",
  cancelled: number,
  reason: "over-max" | "under-min",
  share: number,
  threshold: number,
  dryRun: boolean,
): void {
  state.cancellationsCount += 1;
  state.cancellationsHistory.push({
    seq: state.cancellationsCount,
    t: now,
    instrument: p.name,
    market: p.market,
    side,
    ordersCancelled: cancelled,
    reason,
    sharePct: share,
    thresholdPct: threshold,
    dryRun,
  });
  while (state.cancellationsHistory.length > MAX_CANCELS) state.cancellationsHistory.shift();
}

export const DEFAULT_INSTRUMENTS: Instrument[] = [
  { name: "Amulet", market: "CC-USDC", balance: 100, priceMultiplier: 1, bidsActive: 3, offersActive: 3 },
  { name: "CBTC",   market: "CBTC-CC", balance: 0.005, priceMultiplier: 20000, bidsActive: 2, offersActive: 2 },
  { name: "CETH",   market: "CETH-CC", balance: 0.05, priceMultiplier: 2000, bidsActive: 2, offersActive: 2 },
];
