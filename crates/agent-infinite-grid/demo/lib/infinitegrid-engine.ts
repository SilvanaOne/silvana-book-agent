// Port of agent-infinite-grid placement logic to TypeScript. Mirrors
// crates/agent-infinite-grid/src/main.rs (fn grid_loop).
//
// Rules:
//   Ladder around mid:
//     BID_i   = mid × (1 − stepPct × i / 100)   for i=1..levels
//     OFFER_i = mid × (1 + stepPct × i / 100)   for i=1..levels
//   Every refreshSecs check drift = |mid − gridCenter| / gridCenter × 100.
//   If drift > driftThresholdPct → cancel all active levels, rebuild ladder
//   around current mid. Otherwise leave orders as-is.
//   Fills (synthetic tape): BID fills when mid ≤ bid.price. OFFER fills when
//   mid ≥ offer.price. Filled orders move from `activeOrders` to `history`.

export type OrderType = "BID" | "OFFER";

export type InfiniteGridConfig = Readonly<{
  market: string;
  stepPct: number;              // spacing between levels, in percent
  levels: number;               // number of levels per side (integer)
  quantityPerLevel: number;     // qty on each level
  refreshSecs: number;          // seconds between drift checks
  driftThresholdPct: number;    // rebuild trigger, in percent
  startingPrice: number;        // seed for the price simulator
}>;

export type InfiniteGridOrder = Readonly<{
  seq: number;
  t: number;                    // epoch ms of placement
  side: OrderType;
  level: number;                // 1..levels
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
  gridCenter: number | null;    // mid used to build the current ladder
  lastRefreshAt: number;        // epoch ms of last drift check
  lastDriftPct: number;
  rebuildsCount: number;
  ordersPlaced: number;         // cumulative
  ordersFilled: number;
  ordersCancelled: number;
  realizedPnl: number;
  netInventory: number;         // + long, − short (BID buys, OFFER sells)
  lastFillMessage: string | null;
  activeOrders: InfiniteGridOrder[]; // current ladder
  history: InfiniteGridOrder[];      // filled + cancelled, bounded
};

const MAX_HISTORY = 120;

export function initState(config: InfiniteGridConfig, startPrice: number): InfiniteGridState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    gridCenter: null,
    lastRefreshAt: 0,
    lastDriftPct: 0,
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

/** Applies a tick. Returns state + emitted events. */
export function step(state: InfiniteGridState, price: number, now: number): { state: InfiniteGridState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // 1) Sweep fills against the synthetic tape.
  const stillOpen: InfiniteGridOrder[] = [];
  for (const o of state.activeOrders) {
    if (o.status !== "open") { stillOpen.push(o); continue; }
    const filled = o.side === "BID" ? price <= o.price : price >= o.price;
    if (!filled) { stillOpen.push(o); continue; }
    const filledOrder: InfiniteGridOrder = { ...o, status: "filled", filledAt: now, filledMid: price };
    pushHistory(state, filledOrder);
    state.ordersFilled += 1;
    // Approximate PnL: BID buys at o.price, mark to mid; OFFER sells at o.price, mark to mid.
    const pnl = o.side === "BID" ? (price - o.price) * o.qty : (o.price - price) * o.qty;
    state.realizedPnl += pnl;
    state.netInventory += o.side === "BID" ? o.qty : -o.qty;
    const msg = `FILL L${o.level} ${o.side} ${o.qty} @ ${fmt(o.price)} (mid=${fmt(price)}, pnl=${sgn(pnl)}${fmt(Math.abs(pnl))})`;
    state.lastFillMessage = msg;
    events.push(msg);
  }
  state.activeOrders = stillOpen;

  // 2) Initial build.
  if (state.gridCenter === null) {
    buildLadder(state, price, now, events, /*isRebuild*/ false);
    state.gridCenter = price;
    state.lastRefreshAt = now;
    state.lastDriftPct = 0;
    return { state, events };
  }

  // 3) Drift-driven rebuild — only every refreshSecs.
  const center = state.gridCenter;
  const drift = Math.abs((price - center) / center) * 100;
  state.lastDriftPct = drift;
  const dueForCheck = now - state.lastRefreshAt >= state.config.refreshSecs * 1000;
  if (!dueForCheck) return { state, events };

  state.lastRefreshAt = now;
  if (drift <= state.config.driftThresholdPct) {
    events.push(`REFRESH drift=${drift.toFixed(3)}% ≤ ${state.config.driftThresholdPct}% — keeping ladder`);
    return { state, events };
  }

  // 4) Cancel and rebuild.
  for (const o of state.activeOrders) {
    const cancelled: InfiniteGridOrder = { ...o, status: "cancelled", cancelledAt: now };
    pushHistory(state, cancelled);
    state.ordersCancelled += 1;
  }
  state.activeOrders = [];
  events.push(`REBUILD drift=${drift.toFixed(3)}% > ${state.config.driftThresholdPct}% — recentering @ ${fmt(price)}`);
  buildLadder(state, price, now, events, /*isRebuild*/ true);
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
  const { stepPct, levels, quantityPerLevel } = state.config;
  for (let i = 1; i <= levels; i++) {
    const off = (stepPct * i) / 100;
    const bidPrice = round8(mid * (1 - off));
    const offerPrice = round8(mid * (1 + off));
    state.ordersPlaced += 1;
    const bid: InfiniteGridOrder = {
      seq: state.ordersPlaced, t: now, side: "BID", level: i,
      price: bidPrice, qty: quantityPerLevel, status: "open",
    };
    state.activeOrders.push(bid);
    state.ordersPlaced += 1;
    const offer: InfiniteGridOrder = {
      seq: state.ordersPlaced, t: now, side: "OFFER", level: i,
      price: offerPrice, qty: quantityPerLevel, status: "open",
    };
    state.activeOrders.push(offer);
  }
  events.push(
    `${isRebuild ? "REBUILD" : "BUILD"} ladder around ${fmt(mid)}: ${levels}×BID / ${levels}×OFFER, step ${stepPct}%`,
  );
}

function pushHistory(state: InfiniteGridState, o: InfiniteGridOrder) {
  state.history.push(o);
  if (state.history.length > MAX_HISTORY) state.history.shift();
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
