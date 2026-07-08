// Port of agent-trend-following placement logic to TypeScript. Mirrors
// crates/agent-trend-following/src/main.rs (fn trend_loop).
//
// Rule:
//   fast_ema, slow_ema maintained per tick with α = 2 / (window + 1).
//   After warmup, watch the sign of (fast - slow).
//     prev ≤ 0 and curr > 0  → BID at mid    (bullish crossover)
//     prev ≥ 0 and curr < 0  → OFFER at mid  (bearish crossover)
// Skip if an open order in the intended direction already exists.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";
export type Cross = "up" | "down" | "flat";

export type TrendFollowingConfig = Readonly<{
  market: string;
  fastWindow: number;     // fast EMA period
  slowWindow: number;     // slow EMA period (must be > fastWindow)
  quantity: number;       // qty per entry
  warmupSamples: number;  // ticks observed before any signal
  startingPrice: number;  // seed for the price simulator
}>;

export type TrendFollowingOrder = {
  seq: number;
  t: number;
  type: OrderType;
  price: number;          // == mid at signal time
  qty: number;
  mid: number;
  emaFast: number;
  emaSlow: number;
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
  filledMid?: number;
};

export type TrendFollowingState = {
  config: TrendFollowingConfig;
  status: "monitoring" | "idle";
  alphaFast: number;
  alphaSlow: number;
  currentPrice: number;
  emaFast: number | null;
  emaSlow: number | null;
  prevEmaFast: number | null;
  prevEmaSlow: number | null;
  samples: number;
  signals: number;
  ordersPlaced: number;
  ordersFilled: number;
  realizedPnl: number;
  lastCross: Cross;
  lastCrossAt: number | null;
  orders: TrendFollowingOrder[];
};

const MAX_ORDERS = 60;

export function initState(config: TrendFollowingConfig, startPrice: number): TrendFollowingState {
  return {
    config,
    status: "monitoring",
    alphaFast: 2.0 / (config.fastWindow + 1),
    alphaSlow: 2.0 / (config.slowWindow + 1),
    currentPrice: startPrice,
    emaFast: null,
    emaSlow: null,
    prevEmaFast: null,
    prevEmaSlow: null,
    samples: 0,
    signals: 0,
    ordersPlaced: 0,
    ordersFilled: 0,
    realizedPnl: 0,
    lastCross: "flat",
    lastCrossAt: null,
    orders: [],
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: TrendFollowingState, price: number, now: number): { state: TrendFollowingState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  state.prevEmaFast = state.emaFast;
  state.prevEmaSlow = state.emaSlow;
  state.emaFast = state.emaFast === null ? price : state.alphaFast * price + (1 - state.alphaFast) * state.emaFast;
  state.emaSlow = state.emaSlow === null ? price : state.alphaSlow * price + (1 - state.alphaSlow) * state.emaSlow;
  state.samples += 1;

  // Synthetic fills: BID with price ≤ mid fills (we bought at price, mark to mid),
  // OFFER with price ≥ mid fills (we sold at price, mark to mid).
  for (const o of state.orders) {
    if (o.status !== "open") continue;
    const filled = o.type === "BID" ? price >= o.price : price <= o.price;
    if (!filled) continue;
    o.status = "filled";
    o.filledAt = now;
    o.filledMid = price;
    state.ordersFilled += 1;
    const pnl = o.type === "BID" ? (price - o.price) * o.qty : (o.price - price) * o.qty;
    state.realizedPnl += pnl;
    events.push(
      `FILL #${o.seq} ${o.type} ${o.qty} @ ${fmt(o.price)} (mid=${fmt(price)}, pnl=${sgn(pnl)}${fmt(Math.abs(pnl))})`,
    );
  }

  if (state.samples < state.config.warmupSamples) {
    return { state, events };
  }

  const f = state.emaFast;
  const s = state.emaSlow;
  const pf = state.prevEmaFast;
  const ps = state.prevEmaSlow;
  if (f === null || s === null || pf === null || ps === null) return { state, events };

  const prevDiff = pf - ps;
  const currDiff = f - s;
  const crossUp = prevDiff <= 0 && currDiff > 0;
  const crossDown = prevDiff >= 0 && currDiff < 0;

  if (!crossUp && !crossDown) return { state, events };

  const signal: OrderType = crossUp ? "BID" : "OFFER";
  state.lastCross = crossUp ? "up" : "down";
  state.lastCrossAt = now;

  // Don't stack: skip if an open order already exists in this direction.
  if (state.orders.some((o) => o.status === "open" && o.type === signal)) {
    events.push(
      `${crossUp ? "BULLISH" : "BEARISH"} crossover but open ${signal} exists — skipping`,
    );
    return { state, events };
  }

  const seq = state.ordersPlaced + 1;
  const order: TrendFollowingOrder = {
    seq,
    t: now,
    type: signal,
    price: round8(price),
    qty: state.config.quantity,
    mid: price,
    emaFast: f,
    emaSlow: s,
    status: "open",
  };
  state.orders.push(order);
  state.ordersPlaced = seq;
  state.signals += 1;
  if (state.orders.length > MAX_ORDERS) state.orders.shift();

  events.push(
    `SIGNAL ${signal} #${seq}: ${order.qty} ${state.config.market} @ ${fmt(order.price)} (fast=${fmt(f)}, slow=${fmt(s)}, diff=${sgn(currDiff)}${fmt(Math.abs(currDiff))})`,
  );
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
