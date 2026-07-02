// Port of agent-spot-grid placement logic to TypeScript. Mirrors
// crates/agent-spot-grid/src/main.rs — a minimal passive grid MM:
// on start, stack `bidLevels` bids below mid and `offerLevels` offers above,
// separated by `stepPct`. As the mid walks through a level, that level fills
// and is NOT restored (unlike infinite-grid).

export type OrderType = "BID" | "OFFER";

export type SpotGridConfig = Readonly<{
  market: string;
  midPrice: number;       // grid center
  bidLevels: number;      // number of bid rungs below mid
  offerLevels: number;    // number of offer rungs above mid
  stepPct: number;        // spacing between adjacent rungs, in %
  qtyPerLevel: number;    // quantity for every rung
  startingPrice: number;  // seed for the price simulator
}>;

export type SpotGridOrder = {
  seq: number;
  t: number;              // epoch ms of placement
  side: OrderType;
  price: number;
  qty: number;
  status: "open" | "filled";
  filledAt?: number;
  filledMid?: number;
};

export type SpotGridState = {
  config: SpotGridConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  bidFills: number;
  offerFills: number;
  lastFillAt: number | null;
  lastFillMsg: string | null;
  orders: SpotGridOrder[];
};

export function initState(config: SpotGridConfig, startPrice: number): SpotGridState {
  const orders: SpotGridOrder[] = [];
  const now = Date.now();
  let seq = 1;
  const step = config.stepPct / 100;

  for (let i = 1; i <= config.bidLevels; i++) {
    orders.push({
      seq: seq++,
      t: now,
      side: "BID",
      price: round8(config.midPrice * (1 - step * i)),
      qty: config.qtyPerLevel,
      status: "open",
    });
  }
  for (let i = 1; i <= config.offerLevels; i++) {
    orders.push({
      seq: seq++,
      t: now,
      side: "OFFER",
      price: round8(config.midPrice * (1 + step * i)),
      qty: config.qtyPerLevel,
      status: "open",
    });
  }

  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    bidFills: 0,
    offerFills: 0,
    lastFillAt: null,
    lastFillMsg: null,
    orders,
  };
}

/** Advances the sim by one tick. BIDs fill when mid <= their price, OFFERs when mid >= their price. */
export function step(state: SpotGridState, price: number, now: number): { state: SpotGridState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  for (const o of state.orders) {
    if (o.status !== "open") continue;
    const hit = o.side === "BID" ? price <= o.price : price >= o.price;
    if (!hit) continue;
    o.status = "filled";
    o.filledAt = now;
    o.filledMid = price;
    if (o.side === "BID") state.bidFills += 1;
    else state.offerFills += 1;
    state.lastFillAt = now;
    const msg = `FILL ${o.side.toLowerCase()} #${o.seq} ${o.qty} @ ${fmt(o.price)} (mid=${fmt(price)})`;
    state.lastFillMsg = msg;
    events.push(msg);
  }

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
