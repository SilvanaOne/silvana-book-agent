// Port of agent-hedging-counter-position placement logic to TypeScript. Mirrors
// crates/agent-hedging-counter-position/src/main.rs (fn hedge_loop).
//
// Rule per cycle:
//   delta = balance - target
//   |delta| <= tolerance → no hedge
//   delta > 0            → OFFER qty = delta * hedge_fraction @ mid (sell surplus)
//   delta < 0            → BID   qty = |delta| * hedge_fraction @ mid (buy back)
// At most one open hedge per side at a time.
//
// The demo also simulates exposure drift (external strategies moving the
// unlocked balance up/down each tick) so the operator can watch the hedger
// react in real time.

export type OrderType = "BID" | "OFFER";

export type HedgingConfig = Readonly<{
  exposureInstrument: string;   // e.g. "Amulet"
  hedgeMarket: string;          // e.g. "CC-USDC"
  targetBalance: number;        // desired unlocked balance level
  tolerance: number;            // symmetric band around target (>= 0)
  hedgeFraction: number;        // (0, 1] — how much of the deviation per cycle
  checkIntervalSecs: number;    // seconds between hedge decisions
  exposureDriftPerTick: number; // stddev of Gaussian noise applied to balance
  startingBalance: number;
  startingPrice: number;
}>;

export type HedgeOrder = {
  seq: number;
  t: number;              // epoch ms of placement
  side: OrderType;
  price: number;          // == mid at placement
  qty: number;
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
  filledPrice?: number;
};

export type HedgingState = {
  config: HedgingConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  currentBalance: number;
  ordersPlaced: number;
  ordersFilled: number;
  hedgesCount: number;
  lastHedgeSide: OrderType | null;
  lastCheckAt: number | null;
  nextCheckAt: number;
  orders: HedgeOrder[];
};

const MAX_ORDERS = 60;

export function initState(config: HedgingConfig, now: number): HedgingState {
  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    currentBalance: config.startingBalance,
    ordersPlaced: 0,
    ordersFilled: 0,
    hedgesCount: 0,
    lastHedgeSide: null,
    lastCheckAt: null,
    nextCheckAt: now + config.checkIntervalSecs * 1000,
    orders: [],
  };
}

// Box-Muller sample from N(0,1)
function gaussian(): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  return Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: HedgingState,
  price: number,
  now: number,
): { state: HedgingState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // 1. Exposure drift — simulate external strategies moving the balance.
  const drift = state.config.exposureDriftPerTick;
  if (drift > 0) {
    state.currentBalance = Math.max(0, state.currentBalance + gaussian() * drift);
  }

  // 2. Fill detection against resting orders.
  //    BID fills when price <= o.price (someone crossed our bid down).
  //    OFFER fills when price >= o.price (someone crossed our offer up).
  //    Filled BID means we bought → balance += qty.
  //    Filled OFFER means we sold → balance -= qty.
  for (const o of state.orders) {
    if (o.status !== "open") continue;
    const filled = o.side === "BID" ? price <= o.price : price >= o.price;
    if (!filled) continue;
    o.status = "filled";
    o.filledAt = now;
    o.filledPrice = price;
    state.ordersFilled += 1;
    if (o.side === "BID") {
      state.currentBalance += o.qty;
    } else {
      state.currentBalance = Math.max(0, state.currentBalance - o.qty);
    }
    events.push(
      `FILL #${o.seq} ${o.side} ${fmtQty(o.qty)} @ ${fmtPx(o.price)} → balance ${fmtQty(state.currentBalance)}`,
    );
  }

  // 3. Hedge decision — only every checkIntervalSecs.
  if (now < state.nextCheckAt) return { state, events };
  state.lastCheckAt = now;
  state.nextCheckAt = now + state.config.checkIntervalSecs * 1000;

  const target = state.config.targetBalance;
  const tol = state.config.tolerance;
  const delta = state.currentBalance - target;

  if (Math.abs(delta) <= tol) {
    events.push(
      `check: balance ${fmtQty(state.currentBalance)} within tolerance (target=${fmtQty(target)}±${fmtQty(tol)}) — no hedge`,
    );
    return { state, events };
  }

  const side: OrderType = delta > 0 ? "OFFER" : "BID";
  // Skip if we already have an open hedge in this direction.
  if (state.orders.some((o) => o.status === "open" && o.side === side)) {
    events.push(`check: open ${side} hedge already present — skipping`);
    return { state, events };
  }

  const qty = round8(Math.abs(delta) * state.config.hedgeFraction);
  if (qty <= 0) return { state, events };

  const order: HedgeOrder = {
    seq: state.ordersPlaced + 1,
    t: now,
    side,
    price: round8(price),
    qty,
    status: "open",
  };
  state.orders.push(order);
  state.ordersPlaced = order.seq;
  state.hedgesCount += 1;
  state.lastHedgeSide = side;
  if (state.orders.length > MAX_ORDERS) state.orders.shift();

  events.push(
    `HEDGE ${side} #${order.seq}: ${fmtQty(qty)} ${state.config.hedgeMarket} @ ${fmtPx(order.price)} ` +
      `(balance=${fmtQty(state.currentBalance)}, delta=${sgn(delta)}${fmtQty(Math.abs(delta))}, ` +
      `fraction=${state.config.hedgeFraction})`,
  );
  return { state, events };
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmtPx(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

function fmtQty(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 3 : 4;
  return n.toFixed(digits);
}

function sgn(n: number): string {
  return n >= 0 ? "+" : "-";
}
