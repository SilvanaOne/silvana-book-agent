import { initState, step, type InfiniteGridConfig, type InfiniteGridState } from "./infinitegrid-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; ema: number | null }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  infinitegrid: InfiniteGridState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __infinitegridDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__infinitegridDemoStore) {
    g.__infinitegridDemoStore = {
      infinitegrid: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__infinitegridDemoStore;
}

export function startInfiniteGrid(config: InfiniteGridConfig): InfiniteGridState {
  const store = getStore();
  stopTimer(store);
  store.infinitegrid = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, ema: null }];
  store.events = [
    {
      t: Date.now(),
      message: `Infinite Grid started: ${config.market}, step ${config.stepPct}%, ${config.levels} levels/side, qty ${config.quantityPerLevel}, refresh ${config.refreshSecs}s, drift ${config.driftThresholdPct}%`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.infinitegrid;
}

export function stopInfiniteGrid(): void {
  const store = getStore();
  stopTimer(store);
  if (store.infinitegrid && store.infinitegrid.status === "monitoring") {
    store.infinitegrid.status = "idle";
    store.events.push({ t: Date.now(), message: "Infinite Grid stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.infinitegrid = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.infinitegrid && store.infinitegrid.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { infinitegrid: store.infinitegrid, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.infinitegrid || store.infinitegrid.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.infinitegrid.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.infinitegrid, price, now);
  store.infinitegrid = state;
  store.ticks.push({ t: now, price, ema: state.gridCenter });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
