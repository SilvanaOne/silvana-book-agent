// Port of agent-mean-reversion-bollinger placement logic to TypeScript. Mirrors
// crates/agent-mean-reversion-bollinger/src/main.rs (fn mr_loop).
//
// Rule:
//   Maintain a rolling window of the last `window` mid samples.
//   sma    = mean of the window
//   stddev = sample standard deviation (Bessel-corrected, /(n-1))
//   upper  = sma + k × stddev
//   lower  = sma − k × stddev
//   mid ≥ upper → OFFER at sma  (price too high, expect to revert down)
//   mid ≤ lower → BID   at sma  (price too low,  expect to revert up)
// At most one open reversion order per side at a time.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";

export type MrConfig = Readonly<{
  market: string;
  window: number;          // number of samples in the rolling window
  k: number;               // Bollinger multiplier (bands = sma ± k·stddev)
  quantity: number;        // qty per reversion order
  warmupSamples: number;   // skip signals until this many samples observed
  startingPrice: number;   // seed for the price simulator
}>;

export type MrOrder = Readonly<{
  seq: number;
  t: number;               // epoch ms of placement
  type: OrderType;
  price: number;           // == sma at signal time
  qty: number;
  mid: number;             // mid observed at signal time
  sma: number;             // sma at signal time
  stddev: number;          // sample stddev at signal time
  upper: number;           // upper band at signal time
  lower: number;           // lower band at signal time
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
}>;

export type MrState = {
  config: MrConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  window: number[];        // rolling buffer of last `window` mids
  sma: number | null;
  stddev: number | null;
  upper: number | null;
  lower: number | null;
  samples: number;
  signals: number;
  ordersPlaced: number;
  ordersFilled: number;
  realizedPnl: number;     // profit against snap-back fills (quote units)
  lastMid: number;
  orders: MrOrder[];       // recent orders (bounded elsewhere)
};

const MAX_ORDERS = 60;

export function initState(config: MrConfig, startPrice: number): MrState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    window: [],
    sma: null,
    stddev: null,
    upper: null,
    lower: null,
    samples: 0,
    signals: 0,
    ordersPlaced: 0,
    ordersFilled: 0,
    realizedPnl: 0,
    lastMid: startPrice,
    orders: [],
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: MrState, price: number, now: number): { state: MrState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.lastMid = price;

  // Push into the rolling window.
  if (state.window.length >= state.config.window) state.window.shift();
  state.window.push(price);
  state.samples += 1;

  // Sweep: any live BID whose price >= mid, or OFFER whose price <= mid,
  // is considered filled by the synthetic tape. Realized PnL is booked
  // against the mid at fill time (a rough proxy — the demo is a teaching
  // model, not a backtest).
  for (const o of state.orders) {
    if (o.status !== "open") continue;
    const filled = o.type === "BID" ? price >= o.price : price <= o.price;
    if (!filled) continue;
    (o as { status: "open" | "filled" | "cancelled" }).status = "filled";
    (o as { filledAt?: number }).filledAt = now;
    state.ordersFilled += 1;
    // Snap-back PnL: BID buys at o.price and we mark to mid = price;
    // OFFER sells at o.price and we mark to mid = price.
    const pnl = o.type === "BID" ? (price - o.price) * o.qty : (o.price - price) * o.qty;
    state.realizedPnl += pnl;
    events.push(
      `FILL #${o.seq} ${o.type} ${o.qty} @ ${fmt(o.price)} (mid=${fmt(price)}, snap-back pnl=${sgn(pnl)}${fmt(Math.abs(pnl))})`,
    );
  }

  // Not enough data for bands, or still in warmup.
  if (state.window.length < 2 || state.samples < state.config.warmupSamples) {
    state.sma = state.window.length > 0 ? state.window.reduce((a, b) => a + b, 0) / state.window.length : null;
    state.stddev = null;
    state.upper = null;
    state.lower = null;
    return { state, events };
  }

  const n = state.window.length;
  const sma = state.window.reduce((a, b) => a + b, 0) / n;
  const variance = state.window.reduce((acc, x) => acc + (x - sma) * (x - sma), 0) / (n - 1);
  const stddev = Math.sqrt(variance);
  const upper = sma + state.config.k * stddev;
  const lower = sma - state.config.k * stddev;
  state.sma = sma;
  state.stddev = stddev;
  state.upper = upper;
  state.lower = lower;

  const signal: OrderType | null = price >= upper ? "OFFER" : price <= lower ? "BID" : null;
  if (!signal) return { state, events };

  // Don't stack: skip if an open order already exists in this direction.
  if (state.orders.some((o) => o.status === "open" && o.type === signal)) {
    return { state, events };
  }

  const seq = state.ordersPlaced + 1;
  const order: MrOrder = {
    seq,
    t: now,
    type: signal,
    price: round8(sma),
    qty: state.config.quantity,
    mid: price,
    sma,
    stddev,
    upper,
    lower,
    status: "open",
  };
  state.orders.push(order);
  state.ordersPlaced = seq;
  state.signals += 1;
  if (state.orders.length > MAX_ORDERS) state.orders.shift();

  events.push(
    `SIGNAL ${signal} #${seq}: ${order.qty} ${state.config.market} @ ${fmt(order.price)} (mid=${fmt(price)}, sma=${fmt(sma)}, stddev=${fmt(stddev)}, bands=[${fmt(lower)}, ${fmt(upper)}])`,
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
