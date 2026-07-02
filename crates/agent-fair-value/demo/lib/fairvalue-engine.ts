// Port of agent-fair-value logic to TypeScript. Mirrors
// crates/agent-fair-value/src/main.rs.
//
// Rule per poll: fetch price from every source, aggregate to a single
// fair value via median / mean / trimmed-mean, publish a record. Demo
// simulates sources as noise around a common "true" mid — different
// noise per source, aggregated to reveal that the aggregate tracks the
// true mid closely.

export type AggMethod = "median" | "mean" | "trimmed-mean";

export type FairValueConfig = Readonly<{
  market: string;
  sources: readonly string[];
  method: AggMethod;
  pollSecs: number;
  startingPrice: number;
  sourceNoisePct: number; // stddev of per-source noise as % (e.g. 0.5 = 0.5%)
}>;

export type SourceQuote = Readonly<{ name: string; price: number; lastAt: number }>;

export type FairValueSample = Readonly<{
  t: number;
  fair: number;
  truth: number;
  sources: readonly { name: string; price: number }[];
}>;

export type FairValueState = {
  config: FairValueConfig;
  status: "monitoring" | "idle";
  ticks: number;                 // total input ticks observed
  sourcePrices: SourceQuote[];
  fairValue: number | null;
  publishedCount: number;
  lastPublishedAt?: number;
  samplesPerCycle: number;       // # sources used in most recent aggregation
  spreadBpsAcrossSources: number;// (max−min)/fair × 10000 in most recent cycle
  history: FairValueSample[];
  startedAt: number;
};

const MAX_HISTORY = 400;

export function initState(config: FairValueConfig, startPrice: number): FairValueState {
  return {
    config,
    status: "monitoring",
    ticks: 0,
    sourcePrices: config.sources.map((name) => ({ name, price: startPrice, lastAt: 0 })),
    fairValue: null,
    publishedCount: 0,
    lastPublishedAt: undefined,
    samplesPerCycle: 0,
    spreadBpsAcrossSources: 0,
    history: [],
    startedAt: Date.now(),
  };
}

/**
 * Applies a tick: `price` is the "true" mid (base GBM). For each source,
 * we generate a noisy sample around it. Every `pollSecs` ticks (assuming
 * 1s tick cadence), the engine aggregates and publishes a fair value.
 */
export function step(state: FairValueState, price: number, now: number): { state: FairValueState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.ticks += 1;

  const noise = state.config.sourceNoisePct / 100;
  // Refresh every source with an independent gaussian offset from the true mid.
  state.sourcePrices = state.sourcePrices.map((s) => {
    const noisy = price * (1 + gaussian() * noise);
    return { name: s.name, price: Math.max(1e-12, noisy), lastAt: now };
  });

  // Publish exactly every pollSecs ticks (assuming 1s tick cadence).
  const shouldPublish = state.ticks % Math.max(1, state.config.pollSecs) === 0;
  if (!shouldPublish) return { state, events };

  const quotes = state.sourcePrices.map((s) => s.price);
  const fair = aggregate(quotes, state.config.method);
  const { lo, hi } = bounds(quotes);
  const spreadBps = fair > 0 ? ((hi - lo) / fair) * 10000 : 0;
  state.fairValue = fair;
  state.publishedCount += 1;
  state.lastPublishedAt = now;
  state.samplesPerCycle = quotes.length;
  state.spreadBpsAcrossSources = spreadBps;

  state.history.push({
    t: now,
    fair,
    truth: price,
    sources: state.sourcePrices.map((s) => ({ name: s.name, price: s.price })),
  });
  if (state.history.length > MAX_HISTORY) state.history.shift();

  const parts = state.sourcePrices.map((s) => `${s.name}=${fmt(s.price)}`).join(", ");
  events.push(
    `FAIR #${state.publishedCount} ${state.config.method}(${parts}) = ${fmt(fair)}  spread=${spreadBps.toFixed(1)}bps`,
  );
  return { state, events };
}

export function aggregate(prices: readonly number[], method: AggMethod): number {
  const sorted = [...prices].sort((a, b) => a - b);
  const n = sorted.length;
  if (n === 0) return 0;
  switch (method) {
    case "mean":
      return sorted.reduce((a, b) => a + b, 0) / n;
    case "median":
      return n % 2 === 1 ? sorted[(n - 1) / 2] : (sorted[n / 2 - 1] + sorted[n / 2]) / 2;
    case "trimmed-mean": {
      if (n <= 2) return sorted.reduce((a, b) => a + b, 0) / n;
      const inner = sorted.slice(1, n - 1);
      return inner.reduce((a, b) => a + b, 0) / inner.length;
    }
  }
}

function bounds(prices: readonly number[]): { lo: number; hi: number } {
  let lo = Number.POSITIVE_INFINITY;
  let hi = Number.NEGATIVE_INFINITY;
  for (const p of prices) {
    if (p < lo) lo = p;
    if (p > hi) hi = p;
  }
  return { lo, hi };
}

function gaussian(): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  return Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
