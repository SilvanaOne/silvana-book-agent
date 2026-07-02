// Port of agent-target-allocation placement logic to TypeScript. Mirrors
// crates/agent-target-allocation/src/main.rs (fn alloc_loop).
//
// Rule (per instrument, each check-interval):
//   value = balance × price(market)
//   deviation = value − targetQuote
//   |deviation| > thresholdQuote  →  place order
//     deviation < 0  →  BID  (under-target, buy more)
//     deviation > 0  →  OFFER (over-target, sell some)
//   qty = |deviation| × rebalanceFraction / price
// One open order per (instrument, direction) at a time — no stacking.

export type OrderType = "BID" | "OFFER";

export type Target = Readonly<{
  instrument: string;
  market: string;
  targetQuote: number;      // desired quote-currency value (e.g. USDC)
  startBalance: number;     // starting balance of the instrument
  priceMultiplier: number;  // instrument-market price = basePrice × multiplier
}>;

export type AllocationOrder = Readonly<{
  seq: number;
  t: number;
  instrument: string;
  market: string;
  side: OrderType;
  qty: number;
  price: number;
  quoteAmount: number;      // qty × price
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
}>;

export type TargetAllocationConfig = Readonly<{
  targets: readonly Target[];
  thresholdQuote: number;
  rebalanceFraction: number;
  checkIntervalSecs: number;
  startingPrice: number;    // base price seed for the simulator
}>;

export type Position = {
  instrument: string;
  market: string;
  balance: number;
  price: number;
  currentQuote: number;
  targetQuote: number;
  deviationQuote: number;
};

export type TargetAllocationState = {
  config: TargetAllocationConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  positions: Position[];
  ordersPlaced: number;
  ordersFilled: number;
  orders: AllocationOrder[];
  lastCheckAt: number | null;
};

const MAX_ORDERS = 30;

export const DEFAULT_TARGETS: readonly Target[] = Object.freeze([
  Object.freeze({ instrument: "Amulet", market: "CC-USDC", targetQuote: 10000, startBalance: 100, priceMultiplier: 1 }),
  Object.freeze({ instrument: "CBTC", market: "CBTC-CC", targetQuote: 20000, startBalance: 0.005, priceMultiplier: 20000 }),
]);

export function initState(config: TargetAllocationConfig, startPrice: number): TargetAllocationState {
  const positions: Position[] = config.targets.map((t) => {
    const price = startPrice * t.priceMultiplier;
    const currentQuote = t.startBalance * price;
    return {
      instrument: t.instrument,
      market: t.market,
      balance: t.startBalance,
      price,
      currentQuote,
      targetQuote: t.targetQuote,
      deviationQuote: currentQuote - t.targetQuote,
    };
  });
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    positions,
    ordersPlaced: 0,
    ordersFilled: 0,
    orders: [],
    lastCheckAt: null,
  };
}

/** Apply one tick. Updates positions from live price + optionally places rebalance orders. */
export function step(state: TargetAllocationState, price: number, now: number): { state: TargetAllocationState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Recompute positions from the new price.
  for (const p of state.positions) {
    const t = state.config.targets.find((x) => x.instrument === p.instrument && x.market === p.market);
    const mult = t ? t.priceMultiplier : 1;
    p.price = price * mult;
    p.currentQuote = p.balance * p.price;
    p.deviationQuote = p.currentQuote - p.targetQuote;
  }

  // Sweep fills: BID fills when instrument-market price drops to ord.price;
  // OFFER fills when price rises to ord.price.
  for (const o of state.orders) {
    if (o.status !== "open") continue;
    const pos = state.positions.find((p) => p.instrument === o.instrument && p.market === o.market);
    if (!pos) continue;
    const filled = o.side === "BID" ? pos.price <= o.price : pos.price >= o.price;
    if (!filled) continue;
    (o as { status: "open" | "filled" | "cancelled" }).status = "filled";
    (o as { filledAt?: number }).filledAt = now;
    state.ordersFilled += 1;
    // Update balance from the fill.
    if (o.side === "BID") pos.balance += o.qty;
    else pos.balance -= o.qty;
    pos.currentQuote = pos.balance * pos.price;
    pos.deviationQuote = pos.currentQuote - pos.targetQuote;
    events.push(
      `FILL #${o.seq} ${o.side} ${fmtQty(o.qty)} ${o.instrument} @ ${fmt(o.price)} (quote=${fmt(o.quoteAmount)})`,
    );
  }

  // Check interval: only reevaluate deviations every checkIntervalSecs.
  const intervalMs = state.config.checkIntervalSecs * 1000;
  if (state.lastCheckAt !== null && now - state.lastCheckAt < intervalMs) {
    return { state, events };
  }
  state.lastCheckAt = now;

  const threshold = state.config.thresholdQuote;
  const frac = state.config.rebalanceFraction;

  for (const pos of state.positions) {
    if (Math.abs(pos.deviationQuote) <= threshold) continue;
    const side: OrderType = pos.deviationQuote < 0 ? "BID" : "OFFER";
    // No stacking: one open order per (instrument, side).
    if (state.orders.some((o) => o.status === "open" && o.instrument === pos.instrument && o.side === side)) continue;
    const closeValue = Math.abs(pos.deviationQuote) * frac;
    const qty = pos.price > 0 ? closeValue / pos.price : 0;
    if (qty <= 0) continue;
    const seq = state.ordersPlaced + 1;
    const order: AllocationOrder = {
      seq,
      t: now,
      instrument: pos.instrument,
      market: pos.market,
      side,
      qty: round8(qty),
      price: round8(pos.price),
      quoteAmount: round8(qty * pos.price),
      status: "open",
    };
    state.orders.push(order);
    state.ordersPlaced = seq;
    if (state.orders.length > MAX_ORDERS) state.orders.shift();
    events.push(
      `REBAL ${side} #${seq}: ${fmtQty(order.qty)} ${pos.instrument} on ${pos.market} @ ${fmt(order.price)} (dev=${sgn(pos.deviationQuote)}${fmt(Math.abs(pos.deviationQuote))} quote)`,
    );
  }

  return { state, events };
}

function round8(n: number): number { return Math.round(n * 1e8) / 1e8; }
function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
function fmtQty(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
function sgn(n: number): string { return n >= 0 ? "+" : "−"; }
