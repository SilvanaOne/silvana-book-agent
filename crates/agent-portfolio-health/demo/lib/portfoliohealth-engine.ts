// Port of agent-portfolio-health snapshot logic to TypeScript. Mirrors
// crates/agent-portfolio-health/src/main.rs (fn report).
//
// The Rust binary is a one-shot read-only reporter — it prints a snapshot of:
//   * balances per instrument (total)
//   * open orders aggregated per market/side (bid_qty, bid_notional,
//     offer_qty, offer_notional, order_count)
//   * pending settlements (count, notional, age)
//   * optional mid-price valuation of the party's net open inventory
//
// The demo simulates that same read-only view over time so the user can
// watch balances drift under simulated fills and see the aggregated
// exposure react.

export type PortfolioHealthInstrument = Readonly<{
  name: string;
  startBalance: number;
}>;

export type PortfolioHealthConfig = Readonly<{
  markets: string[];              // e.g. ["CC-USDC","BTC-USD"]
  instruments: PortfolioHealthInstrument[];
  snapshotIntervalSecs: number;   // cadence of full snapshot events
  startingPrice: number;          // seed for the price simulator (quote per base)
}>;

export type BalanceRow = {
  instrument: string;
  balance: number;
  startBalance: number;
  deltaSinceStart: number;
};

export type OpenOrderRow = {
  market: string;
  side: "BID" | "OFFER";
  qty: number;
  price: number;
  notional: number;
};

export type PendingSettlementRow = {
  id: string;
  market: string;
  notional: number;
  ageSec: number;
  createdAt: number;
};

export type PortfolioHealthState = {
  config: PortfolioHealthConfig;
  status: "monitoring" | "idle";
  currentPrice: number;                 // quote-currency mid used as reference
  ticks: number;                        // total 1s ticks observed
  snapshotsCount: number;               // full snapshots emitted
  lastSnapshotAt: number | null;

  balances: BalanceRow[];
  openOrders: OpenOrderRow[];
  pendingSettlements: PendingSettlementRow[];

  // Mid-price valuation aggregate (sum of balance × instrument-price-in-quote).
  portfolioValueQuote: number;
  startPortfolioValueQuote: number;
};

// Instrument prices are expressed as ratios of currentPrice. Amulet is the
// quote-like coin (~1 × currentPrice), CBTC is roughly 400× currentPrice,
// CETH is roughly 25× currentPrice. Kept simple and deterministic so the
// simulation is easy to reason about.
const INSTRUMENT_PRICE_RATIOS: Record<string, number> = {
  Amulet: 1.0,
  CBTC: 400.0,
  CETH: 25.0,
  USDC: 1.0,
};

function priceOf(instrument: string, mid: number): number {
  const r = INSTRUMENT_PRICE_RATIOS[instrument];
  return typeof r === "number" ? r * mid : mid;
}

export function initState(config: PortfolioHealthConfig): PortfolioHealthState {
  const balances: BalanceRow[] = config.instruments.map((i) => ({
    instrument: i.name,
    balance: i.startBalance,
    startBalance: i.startBalance,
    deltaSinceStart: 0,
  }));

  const openOrders = seedOpenOrders(config.markets, config.startingPrice);
  const pendingSettlements = seedPendingSettlements(config.markets, config.startingPrice);
  const startValue = balances.reduce(
    (s, b) => s + b.balance * priceOf(b.instrument, config.startingPrice),
    0,
  );

  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    ticks: 0,
    snapshotsCount: 0,
    lastSnapshotAt: null,
    balances,
    openOrders,
    pendingSettlements,
    portfolioValueQuote: startValue,
    startPortfolioValueQuote: startValue,
  };
}

function seedOpenOrders(markets: string[], startPrice: number): OpenOrderRow[] {
  const out: OpenOrderRow[] = [];
  for (const m of markets) {
    // Two bids below mid, two offers above — the shape of a light-touch
    // market maker book, purely illustrative.
    for (const [side, mult, qty] of [
      ["BID", 0.995, 3] as const,
      ["BID", 0.99, 5] as const,
      ["OFFER", 1.005, 3] as const,
      ["OFFER", 1.01, 5] as const,
    ]) {
      const price = round8(startPrice * mult);
      out.push({ market: m, side, qty, price, notional: round8(price * qty) });
    }
  }
  return out;
}

function seedPendingSettlements(markets: string[], startPrice: number): PendingSettlementRow[] {
  const now = Date.now();
  return markets.slice(0, 1).map((m, i) => {
    const qty = 4;
    const notional = round8(qty * startPrice);
    return {
      id: `ps-${i + 1}`,
      market: m,
      notional,
      ageSec: 12,
      createdAt: now - 12_000,
    };
  });
}

/** Applies one tick. Returns state + emitted events. */
export function step(
  state: PortfolioHealthState,
  price: number,
  now: number,
): { state: PortfolioHealthState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.ticks += 1;

  // Simulate slow drift in balances — every ~4 ticks we book a fake fill of
  // a small ± delta into a random instrument. This is a teaching model, not
  // a real accounting engine.
  if (state.ticks % 4 === 0 && state.balances.length > 0) {
    const idx = state.ticks % state.balances.length;
    const b = state.balances[idx];
    const drift = (deterministicNoise(state.ticks + idx) - 0.5) * 0.02 * b.startBalance;
    b.balance = round8(Math.max(0, b.balance + drift));
    b.deltaSinceStart = round8(b.balance - b.startBalance);
  }

  // Refresh open orders around the new mid so aggregated notionals move
  // alongside the price. Sides/qty stay stable; only price/notional update.
  for (const o of state.openOrders) {
    const mult = o.side === "BID"
      ? (o.qty === 3 ? 0.995 : 0.99)
      : (o.qty === 3 ? 1.005 : 1.01);
    o.price = round8(price * mult);
    o.notional = round8(o.price * o.qty);
  }

  // Age pending settlements. Occasionally clear one and create a fresh one
  // so the "oldest age" number oscillates.
  for (const p of state.pendingSettlements) {
    p.ageSec = Math.floor((now - p.createdAt) / 1000);
  }
  if (state.ticks % 30 === 0 && state.pendingSettlements.length > 0) {
    const dropped = state.pendingSettlements.shift();
    if (dropped) events.push(`SETTLE ${dropped.id} on ${dropped.market} cleared (notional=${fmt(dropped.notional)})`);
    const market = state.config.markets[0] ?? "CC-USDC";
    const qty = 4;
    state.pendingSettlements.push({
      id: `ps-${state.snapshotsCount + Math.floor(state.ticks / 30) + 10}`,
      market,
      notional: round8(qty * price),
      ageSec: 0,
      createdAt: now,
    });
  }

  // Portfolio value in quote.
  state.portfolioValueQuote = round8(
    state.balances.reduce((s, b) => s + b.balance * priceOf(b.instrument, price), 0),
  );

  // Emit a full snapshot event every snapshotIntervalSecs ticks (1 tick/s).
  const dueTick = state.snapshotsCount === 0
    ? 1
    : state.snapshotsCount * state.config.snapshotIntervalSecs;
  if (state.ticks >= dueTick) {
    state.snapshotsCount += 1;
    state.lastSnapshotAt = now;
    const bidNotional = round8(state.openOrders.filter((o) => o.side === "BID").reduce((s, o) => s + o.notional, 0));
    const offerNotional = round8(state.openOrders.filter((o) => o.side === "OFFER").reduce((s, o) => s + o.notional, 0));
    const pendingNotional = round8(state.pendingSettlements.reduce((s, p) => s + p.notional, 0));
    events.push(
      `SNAPSHOT #${state.snapshotsCount}: pv=${fmt(state.portfolioValueQuote)} bidN=${fmt(bidNotional)} offerN=${fmt(offerNotional)} pendN=${fmt(pendingNotional)} pending=${state.pendingSettlements.length}`,
    );
  }

  return { state, events };
}

// Deterministic pseudo-noise in [0, 1) — tiny LCG so the demo is repeatable.
function deterministicNoise(n: number): number {
  const x = Math.sin(n * 12.9898) * 43758.5453;
  return x - Math.floor(x);
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
