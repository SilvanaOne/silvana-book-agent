// Port of agent-spot-dca-value-averaging placement logic to TypeScript.
// Mirrors crates/agent-spot-dca-value-averaging/src/main.rs — value-averaging
// DCA. Instead of a fixed base quantity per period the loop targets a
// linear cumulative *notional* schedule and closes whatever gap remains
// every cycle: buys more when price is low (a fixed quote value buys more
// base), less when price is high.

export type Side = "buy" | "sell";

export type DcaConfig = Readonly<{
  market: string;
  side: Side;
  valuePerPeriod: number;      // target quote-notional added per cycle
  intervalSecs: number;
  priceOffsetPct: number;      // -0.5 = 0.5% below mid for buys
  maxOrderQuote: number | null; // cap on gap-clearing order per cycle (quote units)
  maxTotalQuote: number | null; // stop when cumulative target reaches this notional
  startingPrice: number;
}>;

export type DcaOrder = Readonly<{
  seq: number;
  t: number;
  price: number;
  qty: number;
  side: Side;
  notional: number;
  targetCumulative: number;
  actualCumulative: number;
}>;

export type DcaState = {
  config: DcaConfig;
  status: "monitoring" | "completed" | "idle";
  currentPrice: number;
  lastOrderAt: number;
  cycle: number;                  // number of cycles elapsed
  targetCumulative: number;       // target notional at last cycle
  placedNotional: number;         // sum of order notionals actually placed
  totalQty: number;               // sum of order qty
  avgPrice: number;
  orderCount: number;
  orders: DcaOrder[];
};

export function initState(config: DcaConfig, startPrice: number): DcaState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    lastOrderAt: 0,
    cycle: 0,
    targetCumulative: 0,
    placedNotional: 0,
    totalQty: 0,
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

  state.cycle += 1;
  const targetCumulative = state.config.valuePerPeriod * state.cycle;
  state.targetCumulative = targetCumulative;
  state.lastOrderAt = now;

  if (state.config.maxTotalQuote !== null && targetCumulative > state.config.maxTotalQuote) {
    state.status = "completed";
    events.push(`VA complete — target cumulative ${targetCumulative} exceeds max_total_quote ${state.config.maxTotalQuote}`);
    return { state, events };
  }

  const offsetFactor = 1 + state.config.priceOffsetPct / 100;
  const orderPrice = round8(price * offsetFactor);
  if (orderPrice <= 0) return { state, events };

  let gap = targetCumulative - state.placedNotional;
  if (state.config.maxOrderQuote !== null && gap > state.config.maxOrderQuote) {
    gap = state.config.maxOrderQuote;
  }
  if (gap <= 0) {
    events.push(`VA cycle ${state.cycle}: no order (target=${fmt(targetCumulative)}, placed=${fmt(state.placedNotional)})`);
    return { state, events };
  }

  const qty = gap / orderPrice;
  const notional = qty * orderPrice;

  const seq = state.orderCount + 1;
  const order: DcaOrder = {
    seq,
    t: now,
    price: orderPrice,
    qty,
    side: state.config.side,
    notional,
    targetCumulative,
    actualCumulative: state.placedNotional + notional,
  };
  state.orders.push(order);
  state.orderCount = seq;
  state.totalQty += qty;
  state.placedNotional += notional;
  state.avgPrice = state.placedNotional / state.totalQty;

  const dir = state.config.side === "buy" ? "BID" : "OFFER";
  events.push(
    `VA #${seq} (cycle ${state.cycle}): ${dir} qty=${fmt(qty)} ${state.config.market} @ ${fmt(orderPrice)} — target=${fmt(targetCumulative)} placed=${fmt(state.placedNotional)}`,
  );

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
