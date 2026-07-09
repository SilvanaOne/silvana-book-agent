// Port of agent-spot-grid-skewed placement logic to TypeScript. Mirrors
// crates/agent-spot-grid-skewed/src/main.rs — a passive grid MM whose per-level
// quantity is skewed by current base-instrument inventory. As fills accumulate
// on one side, that side's future bids/offers shrink and the opposite side grows,
// which unwinds net inventory faster.

export type OrderType = "BID" | "OFFER";

export type SpotGridConfig = Readonly<{
  market: string;
  midPrice: number;         // grid center
  bidLevels: number;        // rungs below mid
  offerLevels: number;      // rungs above mid
  stepPct: number;          // arithmetic spacing between adjacent rungs, %
  baseQuantity: number;     // quantity per level before skew
  startingBalance: number;  // initial base-instrument balance
  targetBalance: number;    // desired base-instrument balance
  alpha: number;            // skew intensity in [0, 1]
  startingPrice: number;
}>;

export type SpotGridOrder = {
  seq: number;
  t: number;
  side: OrderType;
  price: number;
  qty: number;
  levelIndex: number;
  status: "open" | "filled";
  filledAt?: number;
  filledMid?: number;
};

export type SpotGridState = {
  config: SpotGridConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  balance: number;
  skew: number;
  bidMult: number;
  offerMult: number;
  bidFills: number;
  offerFills: number;
  lastFillAt: number | null;
  lastFillMsg: string | null;
  orders: SpotGridOrder[];
};

export function initState(config: SpotGridConfig, startPrice: number): SpotGridState {
  const balance = config.startingBalance;
  const skew = clamp((balance - config.targetBalance) / config.targetBalance, -1, 1);
  const bidMult = Math.max(0, 1 - config.alpha * skew);
  const offerMult = Math.max(0, 1 + config.alpha * skew);

  const orders: SpotGridOrder[] = [];
  const now = Date.now();
  let seq = 1;
  const step = config.stepPct / 100;

  const bidQty = round8(config.baseQuantity * bidMult);
  const offerQty = round8(config.baseQuantity * offerMult);

  for (let i = 1; i <= config.bidLevels; i++) {
    if (bidQty > 0) {
      orders.push({
        seq: seq++,
        t: now,
        side: "BID",
        price: round8(config.midPrice * (1 - step * i)),
        qty: bidQty,
        levelIndex: i,
        status: "open",
      });
    }
  }
  for (let i = 1; i <= config.offerLevels; i++) {
    if (offerQty > 0) {
      orders.push({
        seq: seq++,
        t: now,
        side: "OFFER",
        price: round8(config.midPrice * (1 + step * i)),
        qty: offerQty,
        levelIndex: i,
        status: "open",
      });
    }
  }

  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    balance,
    skew,
    bidMult,
    offerMult,
    bidFills: 0,
    offerFills: 0,
    lastFillAt: null,
    lastFillMsg: null,
    orders,
  };
}

/** Advances the sim by one tick. Fills change the base balance and re-skew subsequent quantities. */
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
    if (o.side === "BID") {
      state.bidFills += 1;
      state.balance += o.qty;
    } else {
      state.offerFills += 1;
      state.balance -= o.qty;
    }
    state.lastFillAt = now;

    state.skew = clamp((state.balance - state.config.targetBalance) / state.config.targetBalance, -1, 1);
    state.bidMult = Math.max(0, 1 - state.config.alpha * state.skew);
    state.offerMult = Math.max(0, 1 + state.config.alpha * state.skew);

    const msg = `FILL ${o.side.toLowerCase()} #${o.seq} L${o.levelIndex} ${o.qty} @ ${fmt(o.price)} (bal=${fmt(state.balance)}, skew=${state.skew.toFixed(3)})`;
    state.lastFillMsg = msg;
    events.push(msg);
  }

  return { state, events };
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
