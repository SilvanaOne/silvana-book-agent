// Port of agent-spot-grid-geometric placement logic to TypeScript. Mirrors
// crates/agent-spot-grid-geometric/src/main.rs — passive grid MM with a
// geometric progression of level offsets. Level i offset is
// baseStepPct/100 × ratio^(i-1), so outer rungs get exponentially wider
// while inner ones stay dense near mid. Fills are one-shot (like the static
// grid) — walked-through rungs are not restored.

export type OrderType = "BID" | "OFFER";

export type SpotGridConfig = Readonly<{
  market: string;
  midPrice: number;        // grid center
  bidLevels: number;       // number of bid rungs below mid
  offerLevels: number;     // number of offer rungs above mid
  baseStepPct: number;     // offset of level 1, in %
  ratio: number;           // >= 1.0: geometric multiplier per additional level
  qtyPerLevel: number;     // quantity for every rung
  startingPrice: number;   // seed for the price simulator
}>;

export type SpotGridOrder = {
  seq: number;
  t: number;
  side: OrderType;
  price: number;
  qty: number;
  levelIndex: number;      // 1..N
  offsetPct: number;       // display: (price - mid)/mid * 100
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

  const geometricOffsets = (levels: number) => {
    const out: number[] = [];
    let step = config.baseStepPct / 100;
    for (let i = 1; i <= levels; i++) {
      out.push(step);
      step *= config.ratio;
    }
    return out;
  };

  for (const [idx, off] of geometricOffsets(config.bidLevels).entries()) {
    orders.push({
      seq: seq++,
      t: now,
      side: "BID",
      price: round8(config.midPrice * (1 - off)),
      qty: config.qtyPerLevel,
      levelIndex: idx + 1,
      offsetPct: -off * 100,
      status: "open",
    });
  }
  for (const [idx, off] of geometricOffsets(config.offerLevels).entries()) {
    orders.push({
      seq: seq++,
      t: now,
      side: "OFFER",
      price: round8(config.midPrice * (1 + off)),
      qty: config.qtyPerLevel,
      levelIndex: idx + 1,
      offsetPct: off * 100,
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
    const msg = `FILL ${o.side.toLowerCase()} #${o.seq} L${o.levelIndex} ${o.qty} @ ${fmt(o.price)} (mid=${fmt(price)})`;
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
