import { initState, step, type MrConfig, type MrState } from "./mr-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{
  t: number;
  price: number;
  sma: number | null;
  upper: number | null;
  lower: number | null;
}>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  mr: MrState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __mrDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__mrDemoStore) {
    g.__mrDemoStore = {
      mr: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__mrDemoStore;
}

export function startMr(config: MrConfig): MrState {
  const store = getStore();
  stopTimer(store);
  store.mr = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, sma: null, upper: null, lower: null }];
  store.events = [
    {
      t: Date.now(),
      message: `Bollinger MR started: ${config.market}, window=${config.window}, k=${config.k}, warmup=${config.warmupSamples}, qty ${config.quantity}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.mr;
}

export function stopMr(): void {
  const store = getStore();
  stopTimer(store);
  if (store.mr && store.mr.status === "monitoring") {
    store.mr.status = "idle";
    store.events.push({ t: Date.now(), message: "MR stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.mr = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.mr && store.mr.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { mr: store.mr, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.mr || store.mr.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.mr.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.mr, price, now);
  store.mr = state;
  store.ticks.push({ t: now, price, sma: state.sma, upper: state.upper, lower: state.lower });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
