import { initState, step, type VolatilityScreeningConfig, type VolatilityScreeningState } from "./volatilityscreening-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{
  t: number;
  price: number;
  realizedVol: number | null; // fraction (e.g. 0.42 = 42% annualized)
  logReturn: number | null;   // log-return this tick, if any
}>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  volatilityscreening: VolatilityScreeningState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __volatilityscreeningDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__volatilityscreeningDemoStore) {
    g.__volatilityscreeningDemoStore = {
      volatilityscreening: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__volatilityscreeningDemoStore;
}

export function startVolatilityScreening(config: VolatilityScreeningConfig): VolatilityScreeningState {
  const store = getStore();
  stopTimer(store);
  store.volatilityscreening = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, realizedVol: null, logReturn: null }];
  store.events = [
    {
      t: Date.now(),
      message: `VOL started: ${config.market}, window=${config.window}, poll=${config.pollSecs}s, periods/year=${config.periodsPerYear}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.volatilityscreening;
}

export function stopVolatilityScreening(): void {
  const store = getStore();
  stopTimer(store);
  if (store.volatilityscreening && store.volatilityscreening.status === "monitoring") {
    store.volatilityscreening.status = "idle";
    store.events.push({ t: Date.now(), message: "VOL stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.volatilityscreening = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.volatilityscreening && store.volatilityscreening.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { volatilityscreening: store.volatilityscreening, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.volatilityscreening || store.volatilityscreening.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const prevPrice = store.volatilityscreening.currentPrice;
  const price = nextPrice(prevPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.volatilityscreening, price, now);
  store.volatilityscreening = state;
  const logReturn = prevPrice > 0 ? Math.log(price / prevPrice) : null;
  store.ticks.push({ t: now, price, realizedVol: state.realizedVolAnnualized, logReturn });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
