// Port of agent-volatility-screening publisher logic to TypeScript. Mirrors
// crates/agent-volatility-screening/src/main.rs (fn run).
//
// Rule:
//   Every tick sample the mid price. If we have a previous price, push
//   log-return = ln(price / prev) into a bounded rolling window of `window`
//   samples. On every publish cycle (every `pollSecs` ticks — in this
//   simulator ticks are one second) compute the sample standard deviation
//   `std = sqrt(sum((r - mean)^2) / (n - 1))` and publish
//   `realized_vol_annualized = std * sqrt(periods_per_year / pollSecs)`.
//   No orders are placed — this is a read-only observability agent.

export type VolatilityScreeningConfig = Readonly<{
  market: string;
  window: number;          // rolling window of log-returns
  pollSecs: number;        // publish cadence in seconds (>=1 for the sim)
  periodsPerYear: number;  // e.g. 31536000 for continuous 1s sampling
  startingPrice: number;   // seed for the price simulator
}>;

export type VolatilityPublication = Readonly<{
  seq: number;
  t: number;                    // epoch ms
  mid: number;
  samples: number;
  meanReturn: number;
  stdReturn: number;
  realizedVolAnnualized: number; // as a fraction (e.g. 0.42 for 42%)
}>;

export type VolatilityScreeningState = {
  config: VolatilityScreeningConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  prevPrice: number | null;
  returnsWindow: number[];       // most-recent last, bounded to config.window
  meanReturn: number | null;
  stdReturn: number | null;
  realizedVolAnnualized: number | null; // fraction, not percent
  logVol: number | null;                // ln(1 + realizedVol) — near equal at small vals
  publishedCount: number;
  ticksObserved: number;
  ticksSinceLastPublish: number;
  lastPublishedAt: number | null;
  windowFull: boolean;
  publications: VolatilityPublication[]; // recent (bounded)
};

const MAX_PUBLICATIONS = 120;

export function initState(
  config: VolatilityScreeningConfig,
  startPrice: number,
): VolatilityScreeningState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    prevPrice: null,
    returnsWindow: [],
    meanReturn: null,
    stdReturn: null,
    realizedVolAnnualized: null,
    logVol: null,
    publishedCount: 0,
    ticksObserved: 0,
    ticksSinceLastPublish: 0,
    lastPublishedAt: null,
    windowFull: false,
    publications: [],
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: VolatilityScreeningState,
  price: number,
  now: number,
): { state: VolatilityScreeningState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.ticksObserved += 1;

  if (state.prevPrice !== null && state.prevPrice > 0) {
    const r = Math.log(price / state.prevPrice);
    state.returnsWindow.push(r);
    while (state.returnsWindow.length > state.config.window) {
      state.returnsWindow.shift();
    }
    state.windowFull = state.returnsWindow.length >= state.config.window;
  }
  state.prevPrice = price;
  state.ticksSinceLastPublish += 1;

  // Publish every `pollSecs` ticks (our simulator produces 1 tick/sec).
  const pollTicks = Math.max(1, Math.floor(state.config.pollSecs));
  if (state.ticksSinceLastPublish < pollTicks) return { state, events };
  state.ticksSinceLastPublish = 0;

  const n = state.returnsWindow.length;
  if (n < 2) {
    // Not enough samples yet — matches Rust `warmup:true` payload.
    return { state, events };
  }

  const mean = state.returnsWindow.reduce((a, b) => a + b, 0) / n;
  const variance =
    state.returnsWindow.reduce((a, r) => a + (r - mean) * (r - mean), 0) / (n - 1);
  const std = Math.sqrt(variance);
  const ann = Math.sqrt(state.config.periodsPerYear / state.config.pollSecs);
  const realized = std * ann;

  state.meanReturn = mean;
  state.stdReturn = std;
  state.realizedVolAnnualized = realized;
  state.logVol = Math.log1p(realized);
  state.publishedCount += 1;
  state.lastPublishedAt = now;

  const pub: VolatilityPublication = {
    seq: state.publishedCount,
    t: now,
    mid: price,
    samples: n,
    meanReturn: mean,
    stdReturn: std,
    realizedVolAnnualized: realized,
  };
  state.publications.push(pub);
  if (state.publications.length > MAX_PUBLICATIONS) state.publications.shift();

  events.push(
    `VOL #${pub.seq}: ${(realized * 100).toFixed(2)}% ann. (window=${n}, sample std=${std.toExponential(3)}, mid=${fmt(price)})`,
  );
  return { state, events };
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
