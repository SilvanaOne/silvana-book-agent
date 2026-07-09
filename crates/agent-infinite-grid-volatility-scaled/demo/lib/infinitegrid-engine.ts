// Port of agent-infinite-grid-volatility-scaled placement logic to TypeScript.
// Mirrors crates/agent-infinite-grid-volatility-scaled/src/main.rs.
//
// Rules:
//   1. Poll mid on the tick clock and append ln(mid_t / mid_{t-1}) to a
//      rolling window of length volWindow.
//   2. sigma = sample stddev of the log-return window.
//   3. step_pct = clamp(step_multiplier × sigma × 100, minStepPct, maxStepPct).
//   4. Every refreshSecs, cancel active orders and rebuild an arithmetic
//      ladder using the freshly-derived step_pct around the current mid.
//   5. BIDs fill when mid ≤ bid.price; OFFERs when mid ≥ offer.price.

export type OrderType = "BID" | "OFFER";

export type InfiniteGridConfig = Readonly<{
  market: string;
  levels: number;
  quantityPerLevel: number;
  volWindow: number;              // # of log-return samples kept
  stepMultiplier: number;         // step_pct = mult × sigma × 100 (before clamp)
  minStepPct: number;
  maxStepPct: number;
  refreshSecs: number;
  startingPrice: number;
}>;

export type InfiniteGridOrder = Readonly<{
  seq: number;
  t: number;
  side: OrderType;
  level: number;
  price: number;
  qty: number;
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
  filledMid?: number;
  cancelledAt?: number;
}>;

export type InfiniteGridState = {
  config: InfiniteGridConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  gridCenter: number | null;
  lastRefreshAt: number;
  logReturns: number[];
  prevSample: number | null;
  sigma: number;
  rawStepPct: number;
  currentStepPct: number;
  rebuildsCount: number;
  ordersPlaced: number;
  ordersFilled: number;
  ordersCancelled: number;
  realizedPnl: number;
  netInventory: number;
  lastFillMessage: string | null;
  activeOrders: InfiniteGridOrder[];
  history: InfiniteGridOrder[];
};

const MAX_HISTORY = 120;

export function initState(config: InfiniteGridConfig, startPrice: number): InfiniteGridState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    gridCenter: null,
    lastRefreshAt: 0,
    logReturns: [],
    prevSample: null,
    sigma: 0,
    rawStepPct: 0,
    currentStepPct: config.minStepPct,
    rebuildsCount: 0,
    ordersPlaced: 0,
    ordersFilled: 0,
    ordersCancelled: 0,
    realizedPnl: 0,
    netInventory: 0,
    lastFillMessage: null,
    activeOrders: [],
    history: [],
  };
}

/** Applies a tick. */
export function step(state: InfiniteGridState, price: number, now: number): { state: InfiniteGridState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // 1) Update vol window (every tick is a "sample" for demo simplicity).
  if (state.prevSample !== null && state.prevSample > 0) {
    state.logReturns.push(Math.log(price / state.prevSample));
    while (state.logReturns.length > state.config.volWindow) state.logReturns.shift();
  }
  state.prevSample = price;
  state.sigma = sampleStddev(state.logReturns);
  state.rawStepPct = state.config.stepMultiplier * state.sigma * 100;
  state.currentStepPct = clamp(state.rawStepPct, state.config.minStepPct, state.config.maxStepPct);

  // 2) Sweep fills against the tape.
  const stillOpen: InfiniteGridOrder[] = [];
  for (const o of state.activeOrders) {
    if (o.status !== "open") { stillOpen.push(o); continue; }
    const filled = o.side === "BID" ? price <= o.price : price >= o.price;
    if (!filled) { stillOpen.push(o); continue; }
    const filledOrder: InfiniteGridOrder = { ...o, status: "filled", filledAt: now, filledMid: price };
    pushHistory(state, filledOrder);
    state.ordersFilled += 1;
    const pnl = o.side === "BID" ? (price - o.price) * o.qty : (o.price - price) * o.qty;
    state.realizedPnl += pnl;
    state.netInventory += o.side === "BID" ? o.qty : -o.qty;
    const msg = `FILL L${o.level} ${o.side} ${o.qty} @ ${fmt(o.price)} (mid=${fmt(price)}, pnl=${sgn(pnl)}${fmt(Math.abs(pnl))})`;
    state.lastFillMessage = msg;
    events.push(msg);
  }
  state.activeOrders = stillOpen;

  // 3) Initial build.
  if (state.gridCenter === null) {
    buildLadder(state, price, now, events, false);
    state.gridCenter = price;
    state.lastRefreshAt = now;
    return { state, events };
  }

  // 4) Time-driven rebuild.
  const dueForRebuild = now - state.lastRefreshAt >= state.config.refreshSecs * 1000;
  if (!dueForRebuild) return { state, events };
  state.lastRefreshAt = now;

  // Cancel active levels
  for (const o of state.activeOrders) {
    const cancelled: InfiniteGridOrder = { ...o, status: "cancelled", cancelledAt: now };
    pushHistory(state, cancelled);
    state.ordersCancelled += 1;
  }
  state.activeOrders = [];
  events.push(`REBUILD @ ${fmt(price)} — sigma=${state.sigma.toFixed(5)} step_pct=${state.currentStepPct.toFixed(3)}%`);
  buildLadder(state, price, now, events, true);
  state.gridCenter = price;
  state.rebuildsCount += 1;
  return { state, events };
}

function buildLadder(
  state: InfiniteGridState,
  mid: number,
  now: number,
  events: string[],
  isRebuild: boolean,
): void {
  const { levels, quantityPerLevel } = state.config;
  const step = state.currentStepPct / 100;
  for (let i = 1; i <= levels; i++) {
    const off = step * i;
    const bidPrice = round8(mid * (1 - off));
    const offerPrice = round8(mid * (1 + off));
    state.ordersPlaced += 1;
    state.activeOrders.push({
      seq: state.ordersPlaced, t: now, side: "BID", level: i,
      price: bidPrice, qty: quantityPerLevel, status: "open",
    });
    state.ordersPlaced += 1;
    state.activeOrders.push({
      seq: state.ordersPlaced, t: now, side: "OFFER", level: i,
      price: offerPrice, qty: quantityPerLevel, status: "open",
    });
  }
  events.push(
    `${isRebuild ? "REBUILD" : "BUILD"} ladder around ${fmt(mid)}: ${levels}×BID / ${levels}×OFFER, step ${state.currentStepPct.toFixed(3)}%`,
  );
}

function pushHistory(state: InfiniteGridState, o: InfiniteGridOrder) {
  state.history.push(o);
  if (state.history.length > MAX_HISTORY) state.history.shift();
}

function sampleStddev(xs: number[]): number {
  const n = xs.length;
  if (n < 2) return 0;
  const mean = xs.reduce((s, v) => s + v, 0) / n;
  const variance = xs.reduce((s, v) => s + (v - mean) ** 2, 0) / (n - 1);
  return Math.sqrt(variance);
}
function clamp(x: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, x));
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
