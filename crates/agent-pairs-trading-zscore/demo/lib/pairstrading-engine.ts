// Port of agent-pairs-trading-zscore placement logic to TypeScript. Mirrors
// crates/agent-pairs-trading-zscore/src/main.rs (fn pairs_loop, z-score variant).
//
// Rule:
//   ratio = mid_A / mid_B
//   push ratio into rolling window of length `window`
//   once window is filled to `warmupSamples`:
//     μ = mean(window), σ = sample_stddev(window)
//     z = (ratio - μ) / σ
//   |z| ≤ exitZ  → clear position_open flag, no entry
//   |z| < entryZ → no entry (dead band)
//   z >  +entryZ → A rich, B cheap: OFFER A + BID  B
//   z <  −entryZ → A cheap, B rich: BID   A + OFFER B
// At most one open order per leg in the intended direction (no stacking).

export type OrderType = "BID" | "OFFER";

export type PairsTradingConfig = Readonly<{
  marketA: string;
  marketB: string;
  window: number;
  entryZ: number;
  exitZ: number;
  quantityA: number;
  quantityB: number;
  warmupSamples: number;
  startingPriceA: number;
  startingPriceB: number;
}>;

export type PairsOrder = {
  seq: number;
  t: number;
  market: string;
  leg: "A" | "B";
  side: OrderType;
  price: number;
  qty: number;
  status: "open" | "filled";
  filledAt?: number;
  filledPrice?: number;
};

export type Signal = "long-A" | "short-A" | "neutral";

export type PairsTradingState = {
  config: PairsTradingConfig;
  status: "monitoring" | "idle";
  priceA: number;
  priceB: number;
  ratio: number;
  window: number[];         // rolling buffer of ratio samples
  samples: number;          // total ticks observed
  mean: number | null;
  sigma: number | null;
  z: number | null;
  signal: Signal;
  positionOpen: boolean;
  signalsCount: number;
  signalsSkipped: number;
  ordersPlaced: number;
  ordersFilled: number;
  realizedPnl: number;
  orders: PairsOrder[];
};

const MAX_ORDERS = 60;

export function initState(config: PairsTradingConfig): PairsTradingState {
  const ratio = config.startingPriceA / config.startingPriceB;
  return {
    config,
    status: "monitoring",
    priceA: config.startingPriceA,
    priceB: config.startingPriceB,
    ratio,
    window: [],
    samples: 0,
    mean: null,
    sigma: null,
    z: null,
    signal: "neutral",
    positionOpen: false,
    signalsCount: 0,
    signalsSkipped: 0,
    ordersPlaced: 0,
    ordersFilled: 0,
    realizedPnl: 0,
    orders: [],
  };
}

/**
 * Applies a tick. Caller feeds a fresh priceA (the "master" walk). Leg B walks
 * independently inside step().
 */
export function step(
  state: PairsTradingState,
  priceA: number,
  now: number,
): { state: PairsTradingState; events: string[] } {
  if (state.status !== "monitoring" || priceA <= 0) return { state, events: [] };
  const events: string[] = [];

  // Independent random walk for leg B.
  const zB = boxMuller();
  const nextB = Math.max(1e-9, state.priceB * (1 + 0.005 * zB));
  state.priceA = priceA;
  state.priceB = nextB;
  state.ratio = state.priceA / state.priceB;
  state.samples += 1;

  state.window.push(state.ratio);
  if (state.window.length > state.config.window) state.window.shift();

  // Fill detection: BID filled if leg price <= o.price, OFFER filled if leg price >= o.price
  for (const o of state.orders) {
    if (o.status !== "open") continue;
    const legPrice = o.leg === "A" ? state.priceA : state.priceB;
    const filled = o.side === "BID" ? legPrice <= o.price : legPrice >= o.price;
    if (!filled) continue;
    o.status = "filled";
    o.filledAt = now;
    o.filledPrice = legPrice;
    state.ordersFilled += 1;
    const pnl = o.side === "BID" ? (legPrice - o.price) * o.qty : (o.price - legPrice) * o.qty;
    state.realizedPnl += pnl;
    events.push(
      `FILL #${o.seq} leg-${o.leg} ${o.side} ${o.qty} @ ${fmt(o.price)} (px=${fmt(legPrice)}, pnl=${sgn(pnl)}${fmt(Math.abs(pnl))})`,
    );
  }

  // Warmup?
  if (state.window.length < state.config.warmupSamples) {
    state.mean = null;
    state.sigma = null;
    state.z = null;
    state.signal = "neutral";
    return { state, events };
  }

  const n = state.window.length;
  const mean = state.window.reduce((a, b) => a + b, 0) / n;
  const variance = state.window.reduce((s, x) => s + (x - mean) ** 2, 0) / (n - 1);
  const sigma = Math.sqrt(variance);
  state.mean = mean;
  state.sigma = sigma;

  if (!(sigma > 0) || n < 3) {
    state.z = null;
    state.signal = "neutral";
    return { state, events };
  }

  const z = (state.ratio - mean) / sigma;
  state.z = z;

  // Exit band clears the position flag.
  if (Math.abs(z) <= state.config.exitZ) {
    if (state.positionOpen) state.positionOpen = false;
    state.signal = "neutral";
    return { state, events };
  }

  // Dead band between exitZ and entryZ.
  if (Math.abs(z) < state.config.entryZ) {
    state.signal = "neutral";
    return { state, events };
  }

  let sideA: OrderType;
  let sideB: OrderType;
  if (z > 0) {
    // A rich → SELL A, BUY B
    sideA = "OFFER";
    sideB = "BID";
    state.signal = "short-A";
  } else {
    // A cheap → BUY A, SELL B
    sideA = "BID";
    sideB = "OFFER";
    state.signal = "long-A";
  }

  // Skip if either leg already has an open order in the intended direction.
  const legAOpen = state.orders.some((o) => o.status === "open" && o.leg === "A" && o.side === sideA);
  const legBOpen = state.orders.some((o) => o.status === "open" && o.leg === "B" && o.side === sideB);
  if (legAOpen || legBOpen) {
    state.signalsSkipped += 1;
    return { state, events };
  }

  const seqA = state.ordersPlaced + 1;
  const seqB = state.ordersPlaced + 2;
  const oA: PairsOrder = {
    seq: seqA,
    t: now,
    market: state.config.marketA,
    leg: "A",
    side: sideA,
    price: round8(state.priceA),
    qty: state.config.quantityA,
    status: "open",
  };
  const oB: PairsOrder = {
    seq: seqB,
    t: now,
    market: state.config.marketB,
    leg: "B",
    side: sideB,
    price: round8(state.priceB),
    qty: state.config.quantityB,
    status: "open",
  };
  state.orders.push(oA, oB);
  state.ordersPlaced = seqB;
  state.signalsCount += 1;
  state.positionOpen = true;
  while (state.orders.length > MAX_ORDERS) state.orders.shift();

  events.push(
    `SIGNAL #${state.signalsCount}: z=${sgn(z)}${z.toFixed(3)} ${sideA} ${oA.qty} ${state.config.marketA} @ ${fmt(oA.price)}  +  ${sideB} ${oB.qty} ${state.config.marketB} @ ${fmt(oB.price)} (ratio=${state.ratio.toFixed(6)}, μ=${mean.toFixed(6)}, σ=${sigma.toFixed(6)})`,
  );
  return { state, events };
}

function boxMuller(): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  return Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
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
