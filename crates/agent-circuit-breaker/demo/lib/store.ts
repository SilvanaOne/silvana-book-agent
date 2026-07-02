import { initState, step, type CircuitBreakerConfig, type CircuitBreakerState } from "./circuitbreaker-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; baseline: number | null; status: "monitoring" | "paused" }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  circuitbreaker: CircuitBreakerState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
  stopped: boolean;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __circuitbreakerDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__circuitbreakerDemoStore) {
    g.__circuitbreakerDemoStore = {
      circuitbreaker: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
      stopped: false,
    };
  }
  return g.__circuitbreakerDemoStore;
}

export function startCircuitBreaker(config: CircuitBreakerConfig): CircuitBreakerState {
  const store = getStore();
  stopTimer(store);
  store.circuitbreaker = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, baseline: null, status: "monitoring" }];
  store.events = [
    {
      t: Date.now(),
      message: `Circuit Breaker armed on ${config.market}: threshold ±${config.maxDeviationPct}%, window ${config.windowSecs}s, pause ${config.pauseSecs}s, starting orders ${config.startingOpenOrders}`,
    },
  ];
  store.priceOverride = null;
  store.stopped = false;
  startTimer(store);
  return store.circuitbreaker;
}

export function stopCircuitBreaker(): void {
  const store = getStore();
  stopTimer(store);
  if (store.circuitbreaker) {
    store.stopped = true;
    store.events.push({ t: Date.now(), message: "Circuit Breaker stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.circuitbreaker = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
  store.stopped = false;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.circuitbreaker && !store.stopped) store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { circuitbreaker: store.circuitbreaker, ticks: store.ticks, events: store.events, walk: store.walk, stopped: store.stopped };
}

function startTimer(store: StoreState) {
  store.timer = setInterval(() => tick(store), 1000);
}
function stopTimer(store: StoreState) {
  if (store.timer) {
    clearInterval(store.timer);
    store.timer = null;
  }
}
function tick(store: StoreState) {
  if (!store.circuitbreaker || store.stopped) {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.circuitbreaker.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.circuitbreaker, price, now);
  store.circuitbreaker = state;
  store.ticks.push({ t: now, price, baseline: state.baseline, status: state.status });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
}
