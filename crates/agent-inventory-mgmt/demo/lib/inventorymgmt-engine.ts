// Port of agent-inventory-mgmt placement logic to TypeScript. Mirrors
// crates/agent-inventory-mgmt/src/main.rs (fn inv_loop).
//
// Rule:
//   band = [target - tolerance, target + tolerance]
//   balance > upper → OFFER (sell) chunk_size at mid × (1 + offset/100)
//   balance < lower → BID   (buy)  chunk_size at mid × (1 + offset/100)
//   otherwise → no action
// Only one open rebalance order in each direction at a time. Fills nudge
// the simulated instrument balance by chunk_size in the appropriate
// direction (BID adds, OFFER subtracts).

export type OrderType = "BID" | "OFFER";

export type InventoryMgmtConfig = Readonly<{
  market: string;
  instrument: string;
  target: number;
  tolerance: number;
  chunkSize: number;
  checkIntervalSecs: number;
  priceOffsetPct: number;
  startingBalance: number;
  startingPrice: number;
}>;

export type InventoryOrder = {
  seq: number;
  t: number;
  side: OrderType;
  price: number;
  qty: number;
  status: "open" | "filled";
  filledAt?: number;
  balanceBefore: number;
  balanceAfter: number;
};

export type InventoryMgmtState = {
  config: InventoryMgmtConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  currentBalance: number;
  ordersPlaced: number;
  ordersFilled: number;
  lastCheckAt: number | null;
  lastSignalDiff: number;   // deviation from target at last check (balance - target)
  orders: InventoryOrder[];
};

const MAX_ORDERS = 60;

export function initState(config: InventoryMgmtConfig): InventoryMgmtState {
  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    currentBalance: config.startingBalance,
    ordersPlaced: 0,
    ordersFilled: 0,
    lastCheckAt: null,
    lastSignalDiff: config.startingBalance - config.target,
    orders: [],
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: InventoryMgmtState, price: number, now: number): { state: InventoryMgmtState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Fill detection: BIDs fill when the tape trades at/below their price
  // (someone offers to us); OFFERs fill when the tape trades at/above
  // their price (someone bids into us). Balance moves by chunkSize.
  for (const o of state.orders) {
    if (o.status !== "open") continue;
    const filled = o.side === "BID" ? price <= o.price : price >= o.price;
    if (!filled) continue;
    o.status = "filled";
    o.filledAt = now;
    const balanceBefore = state.currentBalance;
    if (o.side === "BID") state.currentBalance += o.qty;
    else state.currentBalance -= o.qty;
    o.balanceAfter = state.currentBalance;
    state.ordersFilled += 1;
    events.push(
      `FILL #${o.seq} ${o.side} ${fmt(o.qty)} ${state.config.instrument} @ ${fmt(o.price)} (balance ${fmt(balanceBefore)} → ${fmt(state.currentBalance)})`,
    );
  }

  // Check cadence: only re-evaluate the band every checkIntervalSecs.
  const intervalMs = state.config.checkIntervalSecs * 1000;
  if (state.lastCheckAt !== null && now - state.lastCheckAt < intervalMs) {
    return { state, events };
  }
  state.lastCheckAt = now;

  const { target, tolerance, chunkSize, priceOffsetPct } = state.config;
  const upper = target + tolerance;
  const lower = target - tolerance;
  const bal = state.currentBalance;
  state.lastSignalDiff = bal - target;

  let signal: OrderType | null = null;
  if (bal > upper) signal = "OFFER";
  else if (bal < lower) signal = "BID";

  if (!signal) return { state, events };

  // Don't stack: skip if an open order already exists in this direction.
  if (state.orders.some((o) => o.status === "open" && o.side === signal)) {
    return { state, events };
  }

  const orderPrice = round8(price * (1 + priceOffsetPct / 100));
  // Size the order by min(chunkSize, |diff|) so we don't overshoot the band.
  const diff = Math.abs(bal - target);
  const qty = round8(Math.min(chunkSize, diff));
  if (qty <= 0) return { state, events };

  const seq = state.ordersPlaced + 1;
  const order: InventoryOrder = {
    seq,
    t: now,
    side: signal,
    price: orderPrice,
    qty,
    status: "open",
    balanceBefore: bal,
    balanceAfter: bal,
  };
  state.orders.push(order);
  state.ordersPlaced = seq;
  if (state.orders.length > MAX_ORDERS) state.orders.shift();

  events.push(
    `SIGNAL ${signal} #${seq}: ${fmt(qty)} ${state.config.instrument} @ ${fmt(orderPrice)} (balance=${fmt(bal)} vs target=${fmt(target)} ±${fmt(tolerance)})`,
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
