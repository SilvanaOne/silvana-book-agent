// Port of agent-pairs-trading-ratio-divergence placement logic to TypeScript. Mirrors
// crates/agent-pairs-trading-ratio-divergence/src/main.rs (fn pairs_loop).
//
// Rule:
//   ratio = mid_A / mid_B
//   dev   = (ratio - target) / target
//   dev >  +threshold  → A rich, B cheap: OFFER A + BID  B
//   dev <  -threshold  → A cheap, B rich: BID   A + OFFER B
// At most one open order per leg in the intended direction (no stacking).

export type OrderType = "BID" | "OFFER";

export type PairsTradingConfig = Readonly<{
  marketA: string;
  marketB: string;
  targetRatio: number;
  thresholdPct: number;
  quantityA: number;
  quantityB: number;
  startingPriceA: number;
  startingPriceB: number;
}>;

export type PairsOrder = {
  seq: number;
  t: number;
  market: string;         // "A" or "B" (logical leg id) — the market string kept for display
  leg: "A" | "B";
  side: OrderType;
  price: number;
  qty: number;
  status: "open" | "filled";
  filledAt?: number;
  filledPrice?: number;
};

export type PairsTradingState = {
  config: PairsTradingConfig;
  status: "monitoring" | "idle";
  priceA: number;
  priceB: number;
  ratio: number;
  ratioDeviationPct: number;   // ((ratio - target) / target) * 100
  samples: number;
  signalsCount: number;
  signalsSkipped: number;
  ordersPlaced: number;
  ordersFilled: number;
  realizedPnl: number;         // rough quote-equivalent PnL
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
    ratioDeviationPct: ((ratio - config.targetRatio) / config.targetRatio) * 100,
    samples: 0,
    signalsCount: 0,
    signalsSkipped: 0,
    ordersPlaced: 0,
    ordersFilled: 0,
    realizedPnl: 0,
    orders: [],
  };
}

/**
 * Applies a tick. Caller feeds a fresh priceA (the "master" walk). We generate
 * priceB internally via a lightweight independent walk so the pair actually
 * drifts and the ratio moves.
 */
export function step(
  state: PairsTradingState,
  priceA: number,
  now: number,
): { state: PairsTradingState; events: string[] } {
  if (state.status !== "monitoring" || priceA <= 0) return { state, events: [] };
  const events: string[] = [];

  // Independent random walk for leg B so ratio isn't a straight line.
  const zB = boxMuller();
  const nextB = Math.max(1e-9, state.priceB * (1 + 0.005 * zB));
  state.priceA = priceA;
  state.priceB = nextB;
  state.ratio = state.priceA / state.priceB;
  state.ratioDeviationPct = ((state.ratio - state.config.targetRatio) / state.config.targetRatio) * 100;
  state.samples += 1;

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

  const threshold = state.config.thresholdPct / 100;
  const devFrac = (state.ratio - state.config.targetRatio) / state.config.targetRatio;

  let sideA: OrderType | null = null;
  let sideB: OrderType | null = null;
  if (devFrac > threshold) {
    // A is rich, B is cheap → SELL A, BUY B
    sideA = "OFFER";
    sideB = "BID";
  } else if (devFrac < -threshold) {
    // A is cheap, B is rich → BUY A, SELL B
    sideA = "BID";
    sideB = "OFFER";
  } else {
    return { state, events };
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
  while (state.orders.length > MAX_ORDERS) state.orders.shift();

  events.push(
    `SIGNAL #${state.signalsCount}: ${sideA} ${oA.qty} ${state.config.marketA} @ ${fmt(oA.price)}  +  ${sideB} ${oB.qty} ${state.config.marketB} @ ${fmt(oB.price)} (ratio=${state.ratio.toFixed(6)}, dev=${sgn(devFrac * 100)}${(devFrac * 100).toFixed(3)}%)`,
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
