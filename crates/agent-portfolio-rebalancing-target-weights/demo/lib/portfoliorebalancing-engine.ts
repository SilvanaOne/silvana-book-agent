// Port of agent-portfolio-rebalancing-target-weights placement logic to TypeScript. Mirrors
// crates/agent-portfolio-rebalancing-target-weights/src/main.rs.
//
// Each cycle values the portfolio via live mids, computes current weight per
// instrument, compares against target, and places a rebalance order:
//   under-weight → BID  (buy more of the base instrument)
//   over-weight  → OFFER (sell base)
// Sized as (deviation / currentWeight) × balance × rebalanceFraction.

export type OrderType = "BID" | "OFFER";

export type Target = Readonly<{
  instrument: string;   // e.g. Amulet, CBTC, CETH
  market: string;       // e.g. CC-USDC, CBTC-CC
  targetWeight: number; // 0..1
  startBalance: number; // starting balance in instrument units
}>;

export type PortfolioRebalancingConfig = Readonly<{
  targets: readonly Target[];
  thresholdPct: number;      // e.g. 2.0 (percentage points)
  rebalanceFraction: number; // 0..1
  checkIntervalSecs: number; // seconds between rebalance cycles
  startingPrice: number;     // seed for the price simulator (base mid)
}>;

export type BalanceView = Readonly<{
  instrument: string;
  market: string;
  balance: number;
  price: number;         // simulated per-instrument mid (quote per base)
  valueQuote: number;    // balance × price (all normalized to the common quote)
}>;

export type WeightView = Readonly<{
  instrument: string;
  currentWeight: number;
  targetWeight: number;
  deviationPct: number; // (currentWeight − targetWeight) × 100
}>;

export type PortfolioRebalancingOrder = {
  seq: number;
  t: number;
  instrument: string;
  market: string;
  side: OrderType;
  qty: number;
  price: number;
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
};

export type PortfolioRebalancingState = {
  config: PortfolioRebalancingConfig;
  status: "rebalancing" | "idle";
  currentPrice: number;
  balances: BalanceView[];
  portfolioValueQuote: number;
  weights: WeightView[];
  ordersPlaced: number;
  ordersFilled: number;
  orders: PortfolioRebalancingOrder[];
  lastCheckAt: number | null;
  cyclesRun: number;
};

const MAX_ORDERS = 60;

// Per-instrument multipliers vs the base "starting price" so we can simulate a
// multi-market portfolio from one random walk. Amulet is the reference.
const INSTRUMENT_MULTIPLIERS: Readonly<Record<string, number>> = {
  Amulet: 1,
  CC: 1,
  USDC: 1,
  CBTC: 20000,
  BTC: 20000,
  CETH: 2000,
  ETH: 2000,
};

function multiplierFor(instrument: string): number {
  return INSTRUMENT_MULTIPLIERS[instrument] ?? 1;
}

function priceFor(instrument: string, basePrice: number): number {
  return basePrice * multiplierFor(instrument);
}

export function initState(config: PortfolioRebalancingConfig): PortfolioRebalancingState {
  const balances: BalanceView[] = config.targets.map((t) => {
    const price = priceFor(t.instrument, config.startingPrice);
    return {
      instrument: t.instrument,
      market: t.market,
      balance: t.startBalance,
      price,
      valueQuote: t.startBalance * price,
    };
  });
  const portfolioValueQuote = balances.reduce((s, b) => s + b.valueQuote, 0);
  const weights: WeightView[] = config.targets.map((t, i) => {
    const cw = portfolioValueQuote > 0 ? balances[i].valueQuote / portfolioValueQuote : 0;
    return {
      instrument: t.instrument,
      currentWeight: cw,
      targetWeight: t.targetWeight,
      deviationPct: (cw - t.targetWeight) * 100,
    };
  });
  return {
    config,
    status: "rebalancing",
    currentPrice: config.startingPrice,
    balances,
    portfolioValueQuote,
    weights,
    ordersPlaced: 0,
    ordersFilled: 0,
    orders: [],
    lastCheckAt: null,
    cyclesRun: 0,
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: PortfolioRebalancingState,
  price: number,
  now: number,
): { state: PortfolioRebalancingState; events: string[] } {
  if (state.status !== "rebalancing" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Recompute simulated prices per instrument.
  const priceByInst = new Map<string, number>();
  for (const t of state.config.targets) {
    priceByInst.set(t.instrument, priceFor(t.instrument, price));
  }

  // Sweep open orders: fill if the mid crosses the resting order price. When a
  // BID fills we grow the base instrument's balance (buy qty). When an OFFER
  // fills we shrink it (sell qty). We don't model quote-side cash separately —
  // this is a teaching demo, not a backtest.
  for (const o of state.orders) {
    if (o.status !== "open") continue;
    const effPrice = priceByInst.get(o.instrument) ?? price;
    const filled = o.side === "BID" ? effPrice <= o.price : effPrice >= o.price;
    if (!filled) continue;
    o.status = "filled";
    o.filledAt = now;
    state.ordersFilled += 1;
    const bal = state.balances.find((b) => b.instrument === o.instrument);
    if (bal) {
      const delta = o.side === "BID" ? o.qty : -o.qty;
      (bal as { balance: number }).balance = Math.max(0, bal.balance + delta);
    }
    events.push(
      `FILL #${o.seq} ${o.side} ${fmt(o.qty)} ${o.instrument} @ ${fmt(o.price)} on ${o.market}`,
    );
  }

  // Refresh balance view prices + values.
  const refreshed: BalanceView[] = state.balances.map((b) => {
    const p = priceByInst.get(b.instrument) ?? b.price;
    return { ...b, price: p, valueQuote: b.balance * p };
  });
  state.balances = refreshed;
  state.portfolioValueQuote = refreshed.reduce((s, b) => s + b.valueQuote, 0);

  // Refresh weights.
  state.weights = state.config.targets.map((t, i) => {
    const cw = state.portfolioValueQuote > 0 ? refreshed[i].valueQuote / state.portfolioValueQuote : 0;
    return {
      instrument: t.instrument,
      currentWeight: cw,
      targetWeight: t.targetWeight,
      deviationPct: (cw - t.targetWeight) * 100,
    };
  });

  // Rebalance cycle gate.
  const intervalMs = state.config.checkIntervalSecs * 1000;
  const due = state.lastCheckAt === null || now - state.lastCheckAt >= intervalMs;
  if (!due) return { state, events };

  state.lastCheckAt = now;
  state.cyclesRun += 1;

  if (state.portfolioValueQuote <= 0) return { state, events };

  for (let i = 0; i < state.config.targets.length; i++) {
    const t = state.config.targets[i];
    const w = state.weights[i];
    const absDev = Math.abs(w.deviationPct);
    if (absDev <= state.config.thresholdPct) continue;

    const bal = refreshed.find((b) => b.instrument === t.instrument);
    if (!bal || bal.price <= 0) continue;

    // Skip stacking: at most one open order per instrument.
    if (state.orders.some((o) => o.status === "open" && o.instrument === t.instrument)) continue;

    // Notional to push = |deviation weight| × rebalanceFraction × portfolioValue.
    const deltaWeight = w.currentWeight - w.targetWeight;
    const notional = Math.abs(deltaWeight) * state.config.rebalanceFraction * state.portfolioValueQuote;
    const qty = notional / bal.price;
    if (qty <= 0) continue;

    // Under-weight → BID (buy base). Over-weight → OFFER (sell base).
    const side: OrderType = deltaWeight < 0 ? "BID" : "OFFER";

    const seq = state.ordersPlaced + 1;
    const order: PortfolioRebalancingOrder = {
      seq,
      t: now,
      instrument: t.instrument,
      market: t.market,
      side,
      qty: round8(qty),
      price: round8(bal.price),
      status: "open",
    };
    state.orders.push(order);
    state.ordersPlaced = seq;
    if (state.orders.length > MAX_ORDERS) state.orders.shift();

    events.push(
      `REBAL ${side} #${seq}: ${fmt(order.qty)} ${t.instrument} @ ${fmt(order.price)} on ${t.market} ` +
        `(cur ${(w.currentWeight * 100).toFixed(2)}% vs tgt ${(w.targetWeight * 100).toFixed(2)}%, dev ${sgn(w.deviationPct)}${w.deviationPct.toFixed(2)}pp)`,
    );
  }

  return { state, events };
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

function sgn(n: number): string {
  return n >= 0 ? "+" : "";
}
