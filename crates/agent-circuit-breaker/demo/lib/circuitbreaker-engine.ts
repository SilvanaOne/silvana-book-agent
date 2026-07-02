// Port of agent-circuit-breaker logic to TypeScript. Mirrors
// crates/agent-circuit-breaker/src/main.rs.
//
// Rule:
//   Rolling baseline = average of mid samples within the last `windowSecs`.
//   deviationPct = ((mid − baseline) / baseline) × 100
//   |deviationPct| > maxDeviationPct  →  TRIP:
//       cancel all this-party's open orders on that market and pause for
//       `pauseSecs`. After the pause, resume monitoring with a fresh baseline.

export type CBStatus = "monitoring" | "paused";

export type CircuitBreakerConfig = Readonly<{
  market: string;
  maxDeviationPct: number; // trigger when |dev| > this
  windowSecs: number;      // rolling window for baseline (seconds)
  pauseSecs: number;       // sleep this long after a trip
  startingPrice: number;   // seed for the price simulator
  startingOpenOrders: number; // simulated open orders on the market at start
}>;

export type BreachEvent = Readonly<{
  seq: number;
  t: number;
  price: number;
  baseline: number;
  deviationPct: number;
  cancelled: number;       // orders cancelled by this trip
  pauseUntil: number;
}>;

export type PauseInterval = Readonly<{ start: number; end: number }>;

export type Sample = Readonly<{ t: number; price: number }>;

export type CircuitBreakerState = {
  config: CircuitBreakerConfig;
  status: CBStatus;
  currentPrice: number;
  baseline: number | null;         // rolling avg of window
  sampleWindow: Sample[];          // bounded to windowSecs
  lastDeviationPct: number;
  breaches: number;
  lastBreachAt: number | null;
  pauseUntil: number | null;
  cancellationsCount: number;      // orders cancelled across lifetime
  openOrdersMock: number;          // simulated live orders on the market
  breachEvents: BreachEvent[];
  pauseIntervals: PauseInterval[]; // for chart shading
};

const MAX_BREACHES = 60;
const MAX_PAUSES = 60;
const WARMUP_MIN_SAMPLES = 3;

export function initState(config: CircuitBreakerConfig, startPrice: number): CircuitBreakerState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    baseline: null,
    sampleWindow: [],
    lastDeviationPct: 0,
    breaches: 0,
    lastBreachAt: null,
    pauseUntil: null,
    cancellationsCount: 0,
    openOrdersMock: Math.max(0, Math.floor(config.startingOpenOrders)),
    breachEvents: [],
    pauseIntervals: [],
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: CircuitBreakerState,
  price: number,
  now: number,
): { state: CircuitBreakerState; events: string[] } {
  if (price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Resume from pause if elapsed.
  if (state.status === "paused") {
    if (state.pauseUntil !== null && now >= state.pauseUntil) {
      const lastPause = state.pauseIntervals[state.pauseIntervals.length - 1];
      if (lastPause && lastPause.end === state.pauseUntil) {
        // finalize interval (already closed at pauseUntil)
      }
      state.status = "monitoring";
      state.pauseUntil = null;
      state.baseline = price;              // reset baseline on re-arm
      state.sampleWindow = [{ t: now, price }];
      state.lastDeviationPct = 0;
      events.push(`RESUME: monitoring re-armed on ${state.config.market}. Baseline reset to ${fmt(price)}.`);
    } else {
      // Still paused; do not update window.
      return { state, events };
    }
  }

  // Push sample and prune anything older than windowSecs.
  state.sampleWindow.push({ t: now, price });
  const cutoff = now - state.config.windowSecs * 1000;
  while (state.sampleWindow.length > 0 && state.sampleWindow[0].t < cutoff) {
    state.sampleWindow.shift();
  }

  // Recompute rolling baseline.
  let sum = 0;
  for (const s of state.sampleWindow) sum += s.price;
  state.baseline = state.sampleWindow.length > 0 ? sum / state.sampleWindow.length : null;

  // Need enough samples before evaluating deviations.
  if (state.sampleWindow.length <= WARMUP_MIN_SAMPLES || state.baseline === null || state.baseline <= 0) {
    return { state, events };
  }

  const baseline = state.baseline;
  const deviationPct = ((price - baseline) / baseline) * 100;
  state.lastDeviationPct = deviationPct;

  if (Math.abs(deviationPct) > state.config.maxDeviationPct) {
    // TRIP.
    const cancelled = state.openOrdersMock;
    state.cancellationsCount += cancelled;
    state.openOrdersMock = 0;
    state.breaches += 1;
    state.lastBreachAt = now;
    state.status = "paused";
    state.pauseUntil = now + state.config.pauseSecs * 1000;

    const breach: BreachEvent = {
      seq: state.breaches,
      t: now,
      price,
      baseline,
      deviationPct,
      cancelled,
      pauseUntil: state.pauseUntil,
    };
    state.breachEvents.push(breach);
    if (state.breachEvents.length > MAX_BREACHES) state.breachEvents.shift();

    state.pauseIntervals.push({ start: now, end: state.pauseUntil });
    if (state.pauseIntervals.length > MAX_PAUSES) state.pauseIntervals.shift();

    events.push(
      `TRIP #${state.breaches}: deviation ${sgn(deviationPct)}${deviationPct.toFixed(3)}% > threshold ±${state.config.maxDeviationPct}%. Cancel-all on ${state.config.market} (${cancelled} orders). Pausing for ${state.config.pauseSecs}s.`,
    );
  }

  return { state, events };
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

function sgn(n: number): string {
  return n >= 0 ? "+" : "";
}
