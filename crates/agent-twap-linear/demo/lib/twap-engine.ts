// Port of agent-twap-linear placement logic to TypeScript. Mirrors
// crates/agent-twap-linear/src/main.rs (fn twap_loop).
//
// Rule:
//   sliceQty      = total / slices
//   intervalMs    = (durationSecs * 1000) / slices
//   nextSliceAt   = startTime + slicesPlaced * intervalMs
//   orderPrice    = mid × (1 + priceOffsetPct / 100)
//   BID  clamp    = min(orderPrice, limitPrice)  (skip if orderPrice > limit)
//   OFFER clamp   = max(orderPrice, limitPrice)  (skip if orderPrice < limit)
// One order per elapsed slot until slicesPlaced === slices, then status=completed.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";

export type TwapConfig = Readonly<{
  market: string;
  side: Side;
  total: number;
  slices: number;
  durationSecs: number;
  priceOffsetPct: number;
  limitPrice: number | null;
  startingPrice: number;
}>;

export type TwapOrder = Readonly<{
  seq: number;
  t: number;
  type: OrderType;
  price: number;
  qty: number;
  mid: number;
  skipped: boolean;
  skipReason?: string;
}>;

export type TwapState = {
  config: TwapConfig;
  status: "monitoring" | "completed" | "idle";
  sliceQty: number;
  intervalMs: number;
  startTime: number;
  currentPrice: number;
  slicesPlaced: number;
  totalPlaced: number;
  avgPrice: number;
  nextSliceAt: number;
  orders: TwapOrder[];
};

const MAX_ORDERS = 200;

export function initState(config: TwapConfig, startPrice: number, now: number): TwapState {
  const sliceQty = config.total / config.slices;
  const intervalMs = (config.durationSecs * 1000) / config.slices;
  return {
    config,
    status: "monitoring",
    sliceQty,
    intervalMs,
    startTime: now,
    currentPrice: startPrice,
    slicesPlaced: 0,
    totalPlaced: 0,
    avgPrice: 0,
    nextSliceAt: now,
    orders: [],
  };
}

/** Applies a tick. Places a slice if the schedule fires. */
export function step(state: TwapState, price: number, now: number): { state: TwapState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  while (
    state.status === "monitoring" &&
    state.slicesPlaced < state.config.slices &&
    now >= state.nextSliceAt
  ) {
    const seq = state.slicesPlaced + 1;
    const type: OrderType = state.config.side === "buy" ? "BID" : "OFFER";
    const rawPrice = price * (1 + state.config.priceOffsetPct / 100);
    let orderPrice = round8(rawPrice);
    let skipped = false;
    let skipReason: string | undefined;

    if (state.config.limitPrice !== null) {
      const lim = state.config.limitPrice;
      if (type === "BID" && orderPrice > lim) {
        skipped = true;
        skipReason = `price ${fmt(orderPrice)} > limit ${fmt(lim)}`;
      } else if (type === "OFFER" && orderPrice < lim) {
        skipped = true;
        skipReason = `price ${fmt(orderPrice)} < limit ${fmt(lim)}`;
      }
    }

    const qty = state.sliceQty;
    const order: TwapOrder = {
      seq,
      t: now,
      type,
      price: orderPrice,
      qty,
      mid: price,
      skipped,
      skipReason,
    };
    state.orders.push(order);
    if (state.orders.length > MAX_ORDERS) state.orders.shift();
    state.slicesPlaced = seq;

    if (skipped) {
      events.push(
        `TWAP slice ${seq}/${state.config.slices} SKIPPED: ${type} @ ${fmt(orderPrice)} — ${skipReason}`,
      );
    } else {
      // Weighted running average of placed fill prices.
      const prevNotional = state.avgPrice * state.totalPlaced;
      state.totalPlaced += qty;
      state.avgPrice = (prevNotional + orderPrice * qty) / state.totalPlaced;
      events.push(
        `TWAP slice ${seq}/${state.config.slices}: ${type} ${qty} ${state.config.market} @ ${fmt(orderPrice)} (mid=${fmt(price)})`,
      );
    }

    state.nextSliceAt = state.startTime + state.slicesPlaced * state.intervalMs;
  }

  if (state.slicesPlaced >= state.config.slices) {
    if (state.status === "monitoring") {
      state.status = "completed";
      events.push(
        `TWAP COMPLETE: ${state.slicesPlaced}/${state.config.slices} slices, filled ${state.totalPlaced}, avg=${fmt(state.avgPrice)}`,
      );
    }
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
