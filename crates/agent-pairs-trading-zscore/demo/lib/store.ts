import { initState, step, type PairsTradingConfig, type PairsTradingState } from "./pairstrading-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{
  t: number;
  priceA: number;
  priceB: number;
  ratio: number;
}>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  pairstrading: PairsTradingState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __pairstradingDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__pairstradingDemoStore) {
    g.__pairstradingDemoStore = {
      pairstrading: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__pairstradingDemoStore;
}

export function startPairsTrading(config: PairsTradingConfig): PairsTradingState {
  const store = getStore();
  stopTimer(store);
  store.pairstrading = initState(config);
  const now = Date.now();
  store.ticks = [{
    t: now,
    priceA: config.startingPriceA,
    priceB: config.startingPriceB,
    ratio: config.startingPriceA / config.startingPriceB,
  }];
  store.events = [
    {
      t: now,
      message: `PT (z-score) started: A=${config.marketA} B=${config.marketB}, window=${config.window} entry_z=${config.entryZ} exit_z=${config.exitZ}, qty A=${config.quantityA} B=${config.quantityB}, warmup=${config.warmupSamples}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.pairstrading;
}

export function stopPairsTrading(): void {
  const store = getStore();
  stopTimer(store);
  if (store.pairstrading && store.pairstrading.status === "monitoring") {
    store.pairstrading.status = "idle";
    store.events.push({ t: Date.now(), message: "PT stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.pairstrading = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.pairstrading && store.pairstrading.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { pairstrading: store.pairstrading, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.pairstrading || store.pairstrading.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  // Master walk applies to leg A. Leg B walks independently inside step().
  const priceA = nextPrice(store.pairstrading.priceA, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.pairstrading, priceA, now);
  store.pairstrading = state;
  store.ticks.push({ t: now, priceA: state.priceA, priceB: state.priceB, ratio: state.ratio });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
