// Port of agent-twap-vwap placement logic to TypeScript. Mirrors
// crates/agent-twap-vwap/src/main.rs (fn twap_loop + fn build_volume_curve).
//
// Rule:
//   weights_i    = normalize(volumeCurve)  // sums to 1.0, len == slices
//   sliceQty_i   = total × weights_i       // per-slot quantity (variable)
//   intervalMs   = (durationSecs * 1000) / slices
//   nextSliceAt  = startTime + slicesPlaced * intervalMs
//   orderPrice   = mid × (1 + priceOffsetPct / 100)
//   BID  clamp   = min(orderPrice, limitPrice)  (skip if orderPrice > limit)
//   OFFER clamp  = max(orderPrice, limitPrice)  (skip if orderPrice < limit)
// One order per elapsed slot until slicesPlaced === slices, then status=completed.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";

export type VolumeCurvePreset = "u-shaped" | "linear" | "front-loaded";

export type TwapConfig = Readonly<{
  market: string;
  side: Side;
  total: number;
  slices: number;
  durationSecs: number;
  priceOffsetPct: number;
  limitPrice: number | null;
  startingPrice: number;
  /** Raw curve spec — preset name or comma list; kept for display. */
  volumeCurveSpec: string;
  /** Normalized weights, length === slices, sums to 1.0. */
  volumeCurve: readonly number[];
}>;

export type TwapOrder = Readonly<{
  seq: number;
  t: number;
  type: OrderType;
  price: number;
  qty: number;
  weight: number;
  mid: number;
  skipped: boolean;
  skipReason?: string;
}>;

export type TwapState = {
  config: TwapConfig;
  status: "monitoring" | "completed" | "idle";
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

/**
 * Build a normalized weight vector of the given length from a preset name or
 * a comma-separated list of positive numbers. Throws on any invalid input.
 */
export function buildVolumeCurve(spec: string, slices: number): number[] {
  if (!Number.isInteger(slices) || slices < 1) {
    throw new Error(`slices must be an integer >= 1, got ${slices}`);
  }
  const n = slices;
  const trimmed = spec.trim().toLowerCase();
  let raw: number[];
  if (trimmed === "u-shaped" || trimmed === "u_shaped" || trimmed === "u") {
    if (n === 1) {
      raw = [1];
    } else {
      const center = (n - 1) / 2;
      raw = [];
      for (let i = 0; i < n; i++) {
        const d = (i - center) / Math.max(center, 1);
        raw.push(1 + 3 * d * d);
      }
    }
  } else if (trimmed === "linear" || trimmed === "flat" || trimmed === "uniform") {
    raw = new Array(n).fill(1);
  } else if (
    trimmed === "front-loaded" ||
    trimmed === "front_loaded" ||
    trimmed === "front" ||
    trimmed === "decreasing"
  ) {
    raw = [];
    for (let i = 0; i < n; i++) raw.push(n - i);
  } else {
    const parts = trimmed.split(",").map((s) => s.trim()).filter((s) => s.length > 0);
    if (parts.length !== n) {
      throw new Error(`volumeCurve length ${parts.length} does not match slices ${n}`);
    }
    raw = parts.map((p) => {
      const v = Number(p);
      if (!Number.isFinite(v) || v <= 0) {
        throw new Error(`volumeCurve weight '${p}' must be a positive number`);
      }
      return v;
    });
  }

  const sum = raw.reduce((a, b) => a + b, 0);
  if (sum <= 0) throw new Error("volumeCurve weights sum to zero");
  return raw.map((w) => w / sum);
}

export function initState(config: TwapConfig, startPrice: number, now: number): TwapState {
  const intervalMs = (config.durationSecs * 1000) / config.slices;
  return {
    config,
    status: "monitoring",
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
    const idx = state.slicesPlaced;
    const seq = idx + 1;
    const weight = state.config.volumeCurve[idx] ?? 0;
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

    const qty = round8(state.config.total * weight);
    const order: TwapOrder = {
      seq,
      t: now,
      type,
      price: orderPrice,
      qty,
      weight,
      mid: price,
      skipped,
      skipReason,
    };
    state.orders.push(order);
    if (state.orders.length > MAX_ORDERS) state.orders.shift();
    state.slicesPlaced = seq;

    if (skipped) {
      events.push(
        `VWAP slice ${seq}/${state.config.slices} SKIPPED (w=${(weight * 100).toFixed(1)}%): ${type} @ ${fmt(orderPrice)} — ${skipReason}`,
      );
    } else {
      // Weighted running average of placed fill prices.
      const prevNotional = state.avgPrice * state.totalPlaced;
      state.totalPlaced += qty;
      state.avgPrice = state.totalPlaced > 0 ? (prevNotional + orderPrice * qty) / state.totalPlaced : 0;
      events.push(
        `VWAP slice ${seq}/${state.config.slices} (w=${(weight * 100).toFixed(1)}%): ${type} ${qty} ${state.config.market} @ ${fmt(orderPrice)} (mid=${fmt(price)})`,
      );
    }

    state.nextSliceAt = state.startTime + state.slicesPlaced * state.intervalMs;
  }

  if (state.slicesPlaced >= state.config.slices) {
    if (state.status === "monitoring") {
      state.status = "completed";
      events.push(
        `VWAP COMPLETE: ${state.slicesPlaced}/${state.config.slices} slices, filled ${round8(state.totalPlaced)}, avg=${fmt(state.avgPrice)}`,
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
