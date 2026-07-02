import { initState, step, type FairValueConfig, type FairValueState } from "./fairvalue-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; fair: number | null; sources: { name: string; price: number }[] }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  fairvalue: FairValueState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __fairvalueDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__fairvalueDemoStore) {
    g.__fairvalueDemoStore = {
      fairvalue: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__fairvalueDemoStore;
}

export function startFairValue(config: FairValueConfig): FairValueState {
  const store = getStore();
  stopTimer(store);
  store.fairvalue = initState(config, config.startingPrice);
  const now = Date.now();
  store.ticks = [
    {
      t: now,
      price: config.startingPrice,
      fair: null,
      sources: config.sources.map((name) => ({ name, price: config.startingPrice })),
    },
  ];
  store.events = [
    {
      t: now,
      message: `Fair-value started: ${config.market}, sources=[${config.sources.join(",")}], method=${config.method}, poll=${config.pollSecs}s, noise=${config.sourceNoisePct}%`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.fairvalue;
}

export function stopFairValue(): void {
  const store = getStore();
  stopTimer(store);
  if (store.fairvalue && store.fairvalue.status === "monitoring") {
    store.fairvalue.status = "idle";
    store.events.push({ t: Date.now(), message: "Fair-value stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.fairvalue = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.fairvalue && store.fairvalue.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { fairvalue: store.fairvalue, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.fairvalue || store.fairvalue.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  // Use last "truth" (base) price as the seed for the next GBM step, not
  // an aggregated fair value.
  const prevTruth = store.ticks.length > 0 ? store.ticks[store.ticks.length - 1].price : store.fairvalue.config.startingPrice;
  const price = nextPrice(prevTruth, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.fairvalue, price, now);
  store.fairvalue = state;
  store.ticks.push({
    t: now,
    price,
    fair: state.fairValue,
    sources: state.sourcePrices.map((s) => ({ name: s.name, price: s.price })),
  });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
