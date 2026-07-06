import { initState, step, type SmartAllocConfig, type SmartAllocState } from "./smartalloc-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  agent: SmartAllocState | null;
  driverPrice: number;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;

const g = globalThis as unknown as { __smartallocDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__smartallocDemoStore) {
    g.__smartallocDemoStore = {
      agent: null,
      driverPrice: 1,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.005 },
      priceOverride: null,
    };
  }
  return g.__smartallocDemoStore;
}

export function startAgent(config: SmartAllocConfig): SmartAllocState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.agent = initState(config, now);
  const firstMarket = Object.keys(config.startingPrices)[0];
  store.driverPrice = firstMarket ? config.startingPrices[firstMarket] : 1;
  store.ticks = [{ t: now, price: store.driverPrice }];
  store.events = [{
    t: now,
    message: `smart-allocation started — ${config.buckets.length} buckets, threshold=${config.bucketThresholdPct}%, fraction=${config.rebalanceFraction}`,
  }];
  store.priceOverride = null;
  startTimer(store);
  return store.agent;
}

export function stopAgent(): void {
  const store = getStore();
  stopTimer(store);
  if (store.agent && store.agent.status === "running") {
    store.agent.status = "idle";
    store.events.push({ t: Date.now(), message: "smart-allocation stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.agent = null;
  store.ticks = [];
  store.events = [];
  store.driverPrice = 1;
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.agent && store.agent.status === "running") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { agent: store.agent, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.agent || store.agent.status !== "running") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  store.driverPrice = nextPrice(store.driverPrice, store.walk, override);
  const now = Date.now();
  const { state, log } = step(store.agent, store.driverPrice, now);
  store.agent = state;
  store.ticks.push({ t: now, price: store.driverPrice });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of log) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
}
