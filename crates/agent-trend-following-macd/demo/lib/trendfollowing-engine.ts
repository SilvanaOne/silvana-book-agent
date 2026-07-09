// Port of agent-trend-following-macd placement logic to TypeScript. Mirrors
// crates/agent-trend-following-macd/src/main.rs (fn macd_loop).
//
// Rule:
//   EMA_fast   with α = 2 / (fast + 1)   over mid samples
//   EMA_slow   with α = 2 / (slow + 1)   over mid samples
//   MACD       = EMA_fast - EMA_slow
//   EMA_signal with α = 2 / (signal + 1) over MACD samples
//   histogram  = MACD - EMA_signal
//   prev sign ≤ 0 and curr > 0  → BID at mid    (bullish crossover)
//   prev sign ≥ 0 and curr < 0  → OFFER at mid  (bearish crossover)
// Skip if an open order in the intended direction already exists.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";
export type Cross = "up" | "down" | "flat";

export type TrendFollowingConfig = Readonly<{
  market: string;
  fastWindow: number;     // fast EMA period
  slowWindow: number;     // slow EMA period (must be > fastWindow)
  signalPeriod: number;   // signal EMA period, over the MACD line
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
  macd: number;
  signal: number;
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
  filledMid?: number;
};

export type TrendFollowingState = {
  config: TrendFollowingConfig;
  status: "monitoring" | "idle";
  alphaFast: number;
  alphaSlow: number;
  alphaSignal: number;
  currentPrice: number;
  emaFast: number | null;
  emaSlow: number | null;
  macd: number | null;
  signal: number | null;
  histogram: number | null;
  prevHistogram: number | null;
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
    alphaSignal: 2.0 / (config.signalPeriod + 1),
    currentPrice: startPrice,
    emaFast: null,
    emaSlow: null,
    macd: null,
    signal: null,
    histogram: null,
    prevHistogram: null,
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

  state.prevHistogram = state.histogram;
  state.emaFast = state.emaFast === null ? price : state.alphaFast * price + (1 - state.alphaFast) * state.emaFast;
  state.emaSlow = state.emaSlow === null ? price : state.alphaSlow * price + (1 - state.alphaSlow) * state.emaSlow;
  const macd = state.emaFast - state.emaSlow;
  state.macd = macd;
  state.signal = state.signal === null ? macd : state.alphaSignal * macd + (1 - state.alphaSignal) * state.signal;
  state.histogram = macd - state.signal;
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

  const hist = state.histogram;
  const prev = state.prevHistogram;
  if (hist === null || prev === null) return { state, events };

  const crossUp = prev <= 0 && hist > 0;
  const crossDown = prev >= 0 && hist < 0;

  if (!crossUp && !crossDown) return { state, events };

  const signal: OrderType = crossUp ? "BID" : "OFFER";
  state.lastCross = crossUp ? "up" : "down";
  state.lastCrossAt = now;

  // Don't stack: skip if an open order already exists in this direction.
  if (state.orders.some((o) => o.status === "open" && o.type === signal)) {
    events.push(
      `${crossUp ? "BULLISH" : "BEARISH"} MACD crossover but open ${signal} exists — skipping`,
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
    macd: macd,
    signal: state.signal ?? 0,
    status: "open",
  };
  state.orders.push(order);
  state.ordersPlaced = seq;
  state.signals += 1;
  if (state.orders.length > MAX_ORDERS) state.orders.shift();

  events.push(
    `SIGNAL ${signal} #${seq}: ${order.qty} ${state.config.market} @ ${fmt(order.price)} (macd=${sgn(macd)}${fmt(Math.abs(macd))}, sig=${sgn(state.signal ?? 0)}${fmt(Math.abs(state.signal ?? 0))}, hist=${sgn(hist)}${fmt(Math.abs(hist))})`,
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
