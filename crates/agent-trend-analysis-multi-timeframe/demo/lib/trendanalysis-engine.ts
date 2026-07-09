// Port of agent-trend-analysis-multi-timeframe to TypeScript. Mirrors
// crates/agent-trend-analysis-multi-timeframe/src/main.rs — a read-only
// confluence publisher that polls mid and maintains three simultaneous
// simple moving averages of different lengths per market:
//   * sma_short over the last `short` samples
//   * sma_mid   over the last `mid`   samples
//   * sma_long  over the last `long`  samples
// Each tick it computes the slope of every SMA versus the previous tick's
// value and derives an `alignment` verdict — aligned_up when all three
// slopes > 0, aligned_down when all three < 0, mixed otherwise.
// No orders, no ledger writes — pure signal publisher.

export type Alignment = "aligned_up" | "aligned_down" | "mixed" | "warmup";

export type TrendAnalysisConfig = Readonly<{
  market: string;
  short: number;         // fast SMA window
  mid: number;           // mid SMA window
  long: number;          // slow SMA window (also buffer capacity)
  startingPrice: number; // simulator seed
}>;

export type TrendAnalysisState = {
  config: TrendAnalysisConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  priceHistory: number[]; // bounded to `long`
  // SMAs — null until buffer >= long.
  smaShort: number | null;
  smaMid: number | null;
  smaLong: number | null;
  // Slopes — null on the first published tick.
  slopeShort: number | null;
  slopeMid: number | null;
  slopeLong: number | null;
  // Previous SMA values, kept internally to compute the next slope.
  prevSmaShort: number | null;
  prevSmaMid: number | null;
  prevSmaLong: number | null;
  alignment: Alignment;
  publishedCount: number;
  lastPublishedAt?: number;
};

export function initState(config: TrendAnalysisConfig, startPrice: number): TrendAnalysisState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    priceHistory: [],
    smaShort: null,
    smaMid: null,
    smaLong: null,
    slopeShort: null,
    slopeMid: null,
    slopeLong: null,
    prevSmaShort: null,
    prevSmaMid: null,
    prevSmaLong: null,
    alignment: "warmup",
    publishedCount: 0,
  };
}

/** Applies a tick — pushes price, recomputes SMAs & slopes, emits an event on alignment change. */
export function step(
  state: TrendAnalysisState,
  price: number,
  now: number,
): { state: TrendAnalysisState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const c = state.config;
  const events: string[] = [];
  state.currentPrice = price;

  // Push into bounded history (cap == long).
  state.priceHistory.push(price);
  while (state.priceHistory.length > c.long) state.priceHistory.shift();

  // Warmup — need at least `long` samples before any SMA is emitted.
  if (state.priceHistory.length < c.long) {
    state.alignment = "warmup";
    return { state, events };
  }

  const smaShort = tailMean(state.priceHistory, c.short);
  const smaMid = tailMean(state.priceHistory, c.mid);
  const smaLong = tailMean(state.priceHistory, c.long);

  const slopeShort = state.prevSmaShort === null ? null : smaShort - state.prevSmaShort;
  const slopeMid = state.prevSmaMid === null ? null : smaMid - state.prevSmaMid;
  const slopeLong = state.prevSmaLong === null ? null : smaLong - state.prevSmaLong;

  const prevAlignment = state.alignment;
  const alignment = classify(slopeShort, slopeMid, slopeLong);

  state.smaShort = smaShort;
  state.smaMid = smaMid;
  state.smaLong = smaLong;
  state.slopeShort = slopeShort;
  state.slopeMid = slopeMid;
  state.slopeLong = slopeLong;
  state.prevSmaShort = smaShort;
  state.prevSmaMid = smaMid;
  state.prevSmaLong = smaLong;
  state.alignment = alignment;
  state.publishedCount += 1;
  state.lastPublishedAt = now;

  const parts: string[] = [
    `MTF #${state.publishedCount}`,
    `mid=${fmt(price)}`,
    `SMA(${c.short})=${fmt(smaShort)}`,
    `SMA(${c.mid})=${fmt(smaMid)}`,
    `SMA(${c.long})=${fmt(smaLong)}`,
    `align=${alignment}`,
  ];
  events.push(parts.join(" "));

  // Synthetic signal event whenever the alignment changes.
  if (alignment !== prevAlignment) {
    events.push(`SIGNAL alignment ${prevAlignment} → ${alignment}`);
  }

  return { state, events };
}

function tailMean(hist: readonly number[], n: number): number {
  const len = hist.length;
  const start = len - n;
  let sum = 0;
  for (let i = start; i < len; i++) sum += hist[i];
  return sum / n;
}

function classify(
  s: number | null,
  m: number | null,
  l: number | null,
): Alignment {
  if (s === null || m === null || l === null) return "warmup";
  if (s > 0 && m > 0 && l > 0) return "aligned_up";
  if (s < 0 && m < 0 && l < 0) return "aligned_down";
  return "mixed";
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
