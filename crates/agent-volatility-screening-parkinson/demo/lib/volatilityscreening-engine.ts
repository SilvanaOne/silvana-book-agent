// Port of agent-volatility-screening-parkinson publisher logic to TypeScript.
// Mirrors crates/agent-volatility-screening-parkinson/src/main.rs (fn run).
//
// Rule:
//   Every simulator tick sample the mid. Update the currently open bar's
//   high/low. When `periodSecs` elapse the bar closes and is appended to a
//   bounded rolling window of the last `window` closed bars. Once >= 2 bars
//   have closed, the Parkinson estimator publishes
//     sigma^2 = (1 / (4 * ln 2)) * mean( ln(H/L)^2 )   // per-period
//     sigma_annualized = sqrt(sigma^2) * sqrt(periodsPerYear)
//   No orders are placed — this is a read-only observability agent.

export type VolatilityScreeningConfig = Readonly<{
  market: string;
  window: number;          // completed high/low bars kept in the rolling buffer
  pollSecs: number;        // ignored in the demo (ticks are 1s); kept for parity
  periodSecs: number;      // length of each closed bar in seconds
  periodsPerYear: number;  // e.g. 525600 for minute bars
  startingPrice: number;   // seed for the price simulator
}>;

export type ClosedBar = Readonly<{ high: number; low: number }>;

export type OpenBar = Readonly<{
  startedAt: number;       // epoch ms when the bar began
  high: number;
  low: number;
  samples: number;
}>;

export type VolatilityPublication = Readonly<{
  seq: number;
  t: number;                     // epoch ms
  mid: number;
  samples: number;
  sigmaPerPeriod: number;
  sigmaAnnualized: number;       // as a fraction (e.g. 0.42 for 42%)
}>;

export type VolatilityScreeningState = {
  config: VolatilityScreeningConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  prevPrice: number | null;
  currentBar: OpenBar | null;
  closedBars: ClosedBar[];       // most-recent last, bounded to config.window
  sigmaPerPeriod: number | null;
  sigmaAnnualized: number | null;
  aggHigh: number | null;
  aggLow: number | null;
  publishedCount: number;
  ticksObserved: number;
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
    currentBar: null,
    closedBars: [],
    sigmaPerPeriod: null,
    sigmaAnnualized: null,
    aggHigh: null,
    aggLow: null,
    publishedCount: 0,
    ticksObserved: 0,
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
  state.prevPrice = state.currentPrice;
  state.currentPrice = price;
  state.ticksObserved += 1;

  const periodMs = Math.max(1, Math.floor(state.config.periodSecs * 1000));

  // Update the currently open bar or start a new one.
  if (state.currentBar === null) {
    state.currentBar = { startedAt: now, high: price, low: price, samples: 1 };
  } else {
    const bar = state.currentBar;
    const closedThisTick = now - bar.startedAt >= periodMs;
    if (closedThisTick) {
      state.closedBars.push({ high: bar.high, low: bar.low });
      while (state.closedBars.length > state.config.window) {
        state.closedBars.shift();
      }
      state.windowFull = state.closedBars.length >= state.config.window;
      events.push(
        `BAR closed H=${fmt(bar.high)} L=${fmt(bar.low)} (samples=${bar.samples}, bars=${state.closedBars.length}/${state.config.window})`,
      );
      state.currentBar = { startedAt: now, high: price, low: price, samples: 1 };
    } else {
      state.currentBar = {
        startedAt: bar.startedAt,
        high: price > bar.high ? price : bar.high,
        low: price < bar.low ? price : bar.low,
        samples: bar.samples + 1,
      };
    }
  }

  const n = state.closedBars.length;
  if (n < 2) return { state, events };

  // Parkinson estimator over the window of closed bars.
  let sumLn2 = 0;
  let valid = 0;
  for (const b of state.closedBars) {
    if (b.high > 0 && b.low > 0) {
      const r = Math.log(b.high / b.low);
      sumLn2 += r * r;
      valid += 1;
    }
  }
  if (valid < 2) return { state, events };
  const meanLn2 = sumLn2 / valid;
  const variancePerPeriod = meanLn2 / (4 * Math.LN2);
  const sigmaPerPeriod = Math.sqrt(Math.max(0, variancePerPeriod));
  const ann = Math.sqrt(state.config.periodsPerYear);
  const sigmaAnnualized = sigmaPerPeriod * ann;

  state.sigmaPerPeriod = sigmaPerPeriod;
  state.sigmaAnnualized = sigmaAnnualized;
  state.aggHigh = state.closedBars.reduce((m, b) => (b.high > m ? b.high : m), -Infinity);
  state.aggLow = state.closedBars.reduce((m, b) => (b.low < m ? b.low : m), Infinity);
  state.publishedCount += 1;
  state.lastPublishedAt = now;

  const pub: VolatilityPublication = {
    seq: state.publishedCount,
    t: now,
    mid: price,
    samples: n,
    sigmaPerPeriod,
    sigmaAnnualized,
  };
  state.publications.push(pub);
  if (state.publications.length > MAX_PUBLICATIONS) state.publications.shift();

  events.push(
    `VOL #${pub.seq}: ${(sigmaAnnualized * 100).toFixed(2)}% ann. (bars=${n}, sigma/period=${sigmaPerPeriod.toExponential(3)}, mid=${fmt(price)})`,
  );
  return { state, events };
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
