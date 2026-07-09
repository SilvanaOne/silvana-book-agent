// Port of agent-portfolio-rebalancing-threshold-bands placement logic to
// TypeScript. Mirrors crates/agent-portfolio-rebalancing-threshold-bands/src/main.rs.
//
// Event-driven counterpart of the Target Weights template:
//   - Each instrument has an upper and lower band around its target weight
//     (expressed in percentage points of the total portfolio weight).
//   - As long as current_weight stays inside
//     [target - lowerBandPct, target + upperBandPct], the agent is idle.
//   - When a band is breached, one order is placed sized to bring the
//     instrument all the way back to target -- not a fractional nudge.
//   - No stacking: skip if an open order in the intended direction already
//     exists on that market.

export type OrderType = "BID" | "OFFER";

export type Target = Readonly<{
  instrument: string;   // e.g. Amulet, CBTC, CETH
  market: string;       // e.g. CC-USDC, CBTC-CC
  targetWeight: number; // 0..1
  startBalance: number; // starting balance in instrument units
}>;

export type PortfolioRebalancingConfig = Readonly<{
  targets: readonly Target[];
  upperBandPct: number;      // percentage points above target
  lowerBandPct: number;      // percentage points below target
  priceOffsetPct: number;    // percent of mid, applied against trade direction
  checkIntervalSecs: number; // seconds between rebalance cycles
  startingPrice: number;     // seed for the price simulator (base mid)
}>;

export type BalanceView = Readonly<{
  instrument: string;
  market: string;
  balance: number;
  price: number;         // simulated per-instrument mid (quote per base)
  valueQuote: number;    // balance x price (all normalized to the common quote)
}>;

export type WeightView = Readonly<{
  instrument: string;
  currentWeight: number;
  targetWeight: number;
  deviationPct: number;     // (currentWeight - targetWeight) * 100
  upperBoundPct: number;    // (targetWeight * 100) + upperBandPct
  lowerBoundPct: number;    // (targetWeight * 100) - lowerBandPct
  breach: "upper" | "lower" | null;
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
  lastActionByInst: Record<string, string>; // instrument -> short label of last action
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

function computeWeightViews(
  targets: readonly Target[],
  balances: readonly BalanceView[],
  portfolioValueQuote: number,
  upperBandPct: number,
  lowerBandPct: number,
): WeightView[] {
  return targets.map((t, i) => {
    const cw = portfolioValueQuote > 0 ? balances[i].valueQuote / portfolioValueQuote : 0;
    const deviationPct = (cw - t.targetWeight) * 100;
    let breach: "upper" | "lower" | null = null;
    if (deviationPct > upperBandPct) breach = "upper";
    else if (deviationPct < -lowerBandPct) breach = "lower";
    return {
      instrument: t.instrument,
      currentWeight: cw,
      targetWeight: t.targetWeight,
      deviationPct,
      upperBoundPct: t.targetWeight * 100 + upperBandPct,
      lowerBoundPct: t.targetWeight * 100 - lowerBandPct,
      breach,
    };
  });
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
  const weights = computeWeightViews(
    config.targets,
    balances,
    portfolioValueQuote,
    config.upperBandPct,
    config.lowerBandPct,
  );
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
    lastActionByInst: {},
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
  // BID fills we grow the tracked instrument's balance (buy qty). When an OFFER
  // fills we shrink it (sell qty). Simulation only -- we don't model quote-side
  // cash separately.
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

  // Refresh weights + band bounds.
  state.weights = computeWeightViews(
    state.config.targets,
    refreshed,
    state.portfolioValueQuote,
    state.config.upperBandPct,
    state.config.lowerBandPct,
  );

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

    if (w.breach === null) continue;

    const bal = refreshed.find((b) => b.instrument === t.instrument);
    if (!bal || bal.price <= 0) continue;

    // Skip stacking: at most one open order per instrument.
    if (state.orders.some((o) => o.status === "open" && o.instrument === t.instrument)) continue;

    // Under-weight (deviation < 0, `lower` breach) -> BID.
    // Over-weight  (deviation > 0, `upper` breach) -> OFFER.
    const side: OrderType = w.breach === "lower" ? "BID" : "OFFER";

    // Single-shot: size the order so it moves current_weight all the way back
    // to target. notional = |deviation weight| * portfolioValue.
    const deltaWeight = w.currentWeight - w.targetWeight;
    const notional = Math.abs(deltaWeight) * state.portfolioValueQuote;
    const qty = notional / bal.price;
    if (qty <= 0) continue;

    // Apply price offset against the trade direction.
    const offsetFrac = state.config.priceOffsetPct / 100;
    const orderPrice = side === "BID" ? bal.price * (1 - offsetFrac) : bal.price * (1 + offsetFrac);

    const seq = state.ordersPlaced + 1;
    const order: PortfolioRebalancingOrder = {
      seq,
      t: now,
      instrument: t.instrument,
      market: t.market,
      side,
      qty: round8(qty),
      price: round8(orderPrice),
      status: "open",
    };
    state.orders.push(order);
    state.ordersPlaced = seq;
    if (state.orders.length > MAX_ORDERS) state.orders.shift();

    const label =
      `${side} #${seq} ${fmt(order.qty)} @ ${fmt(order.price)} ` +
      `(band ${w.breach}, dev ${sgn(w.deviationPct)}${w.deviationPct.toFixed(2)}pp)`;
    state.lastActionByInst[t.instrument] = label;

    events.push(
      `BREACH ${w.breach.toUpperCase()} on ${t.instrument}@${t.market}: ${label}. ` +
        `Single-shot back-to-target: ${(w.currentWeight * 100).toFixed(2)}% -> ${(w.targetWeight * 100).toFixed(2)}%`,
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
