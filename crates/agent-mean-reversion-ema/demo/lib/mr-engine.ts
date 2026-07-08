// Port of agent-mean-reversion placement logic to TypeScript. Mirrors
// crates/agent-mean-reversion/src/main.rs (fn mr_loop).
//
// Rule:
//   diff = (mid - ema) / ema
//   diff >  +dev  → OFFER at ema  (price too high, expect to snap back down)
//   diff <  -dev  → BID   at ema  (price too low,  expect to snap back up)
// At most one open reversion order per side at a time.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";

export type MrConfig = Readonly<{
  market: string;
  emaWindow: number;      // number of samples used by the EMA smoothing
  deviationPct: number;   // trigger threshold in percent, e.g. 1.5
  quantity: number;       // qty per reversion order
  warmupSamples: number;  // skip signals until this many samples observed
  startingPrice: number;  // seed for the price simulator
}>;

export type MrOrder = Readonly<{
  seq: number;
  t: number;              // epoch ms of placement
  type: OrderType;
  price: number;          // == ema at signal time
  qty: number;
  mid: number;            // mid observed at signal time
  ema: number;            // ema at signal time
  diffPct: number;        // (mid - ema)/ema * 100 at signal time
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
}>;

export type MrState = {
  config: MrConfig;
  status: "monitoring" | "idle";
  alpha: number;          // 2 / (emaWindow + 1)
  currentPrice: number;
  ema: number | null;
  samples: number;
  signals: number;
  ordersPlaced: number;
  ordersFilled: number;
  realizedPnl: number;    // profit against snap-back fills (quote units)
  lastDiffPct: number;
  orders: MrOrder[];      // recent orders (bounded elsewhere)
};

const MAX_ORDERS = 60;

export function initState(config: MrConfig, startPrice: number): MrState {
  return {
    config,
    status: "monitoring",
    alpha: 2.0 / (config.emaWindow + 1),
    currentPrice: startPrice,
    ema: null,
    samples: 0,
    signals: 0,
    ordersPlaced: 0,
    ordersFilled: 0,
    realizedPnl: 0,
    lastDiffPct: 0,
    orders: [],
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: MrState, price: number, now: number): { state: MrState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  state.ema = state.ema === null ? price : state.alpha * price + (1 - state.alpha) * state.ema;
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

  const ema = state.ema;
  const diff = (price - ema) / ema;
  state.lastDiffPct = diff * 100;

  if (state.samples < state.config.warmupSamples) {
    return { state, events };
  }

  const dev = state.config.deviationPct / 100;
  const signal: OrderType | null = diff > dev ? "OFFER" : diff < -dev ? "BID" : null;
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
    price: round8(ema),
    qty: state.config.quantity,
    mid: price,
    ema,
    diffPct: diff * 100,
    status: "open",
  };
  state.orders.push(order);
  state.ordersPlaced = seq;
  state.signals += 1;
  if (state.orders.length > MAX_ORDERS) state.orders.shift();

  events.push(
    `SIGNAL ${signal} #${seq}: ${order.qty} ${state.config.market} @ ${fmt(order.price)} (mid=${fmt(price)}, ema=${fmt(ema)}, diff=${sgn(diff * 100)}${(diff * 100).toFixed(3)}%)`,
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
