// Port of agent-spot-dca-scheduled placement logic to TypeScript. Mirrors
// crates/agent-spot-dca-scheduled/src/main.rs (fn dca_loop).

export type Side = "buy" | "sell";

export type DcaConfig = Readonly<{
  market: string;
  side: Side;
  amountPerOrder: number;
  intervalSecs: number;
  priceOffsetPct: number;   // -0.5 = 0.5% below mid
  maxTotal: number | null;  // optional cap on total accumulated qty
  startingPrice: number;    // seed for the price simulator
}>;

export type DcaOrder = Readonly<{
  seq: number;
  t: number;                // epoch ms
  price: number;
  qty: number;
  side: Side;
  notional: number;         // qty * price (in quote units)
}>;

export type DcaState = {
  config: DcaConfig;
  status: "monitoring" | "completed" | "idle";
  currentPrice: number;
  lastOrderAt: number;      // epoch ms
  totalQty: number;         // sum of order qty
  totalNotional: number;    // sum of order notional
  avgPrice: number;         // totalNotional / totalQty
  orderCount: number;
  orders: DcaOrder[];
};

export function initState(config: DcaConfig, startPrice: number): DcaState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    lastOrderAt: 0,
    totalQty: 0,
    totalNotional: 0,
    avgPrice: 0,
    orderCount: 0,
    orders: [],
  };
}

/** Applies a tick. Returns the (possibly-mutated) state and events raised. */
export function step(state: DcaState, price: number, now: number): { state: DcaState; events: string[] } {
  if (state.status !== "monitoring") return { state, events: [] };
  if (price <= 0) return { state, events: [] };

  const events: string[] = [];
  state.currentPrice = price;

  const dueMs = state.config.intervalSecs * 1000;
  if (state.lastOrderAt !== 0 && now - state.lastOrderAt < dueMs) {
    return { state, events };
  }

  // Time to place a new synthetic order.
  const remainingCap = state.config.maxTotal === null ? Infinity : state.config.maxTotal - state.totalQty;
  if (remainingCap <= 0) {
    state.status = "completed";
    events.push(`DCA complete — max_total ${state.config.maxTotal} reached`);
    return { state, events };
  }

  const offsetFactor = 1 + state.config.priceOffsetPct / 100;
  const orderPrice = round8(price * offsetFactor);
  const qty = Math.min(state.config.amountPerOrder, remainingCap);
  const notional = qty * orderPrice;

  const seq = state.orderCount + 1;
  const order: DcaOrder = { seq, t: now, price: orderPrice, qty, side: state.config.side, notional };
  state.orders.push(order);
  state.orderCount = seq;
  state.totalQty += qty;
  state.totalNotional += notional;
  state.avgPrice = state.totalNotional / state.totalQty;
  state.lastOrderAt = now;

  const dir = state.config.side === "buy" ? "BID" : "OFFER";
  events.push(
    `DCA #${seq}: ${dir} ${qty} ${state.config.market} @ ${fmt(orderPrice)}  (mid=${fmt(price)}, offset=${state.config.priceOffsetPct}%)`,
  );

  if (state.config.maxTotal !== null && state.totalQty >= state.config.maxTotal) {
    state.status = "completed";
    events.push(`DCA complete — max_total ${state.config.maxTotal} reached (${state.orderCount} orders, avg ${fmt(state.avgPrice)})`);
  }

  return { state, events };
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 8;
  return n.toFixed(digits);
}
