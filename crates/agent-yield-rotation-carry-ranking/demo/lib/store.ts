import { initState, step, type YieldRotationConfig, type YieldRotationState } from "./yieldrotation-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  yieldrotation: YieldRotationState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __yieldrotationDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__yieldrotationDemoStore) {
    g.__yieldrotationDemoStore = {
      yieldrotation: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.005 },
      priceOverride: null,
    };
  }
  return g.__yieldrotationDemoStore;
}

export function startYieldRotation(config: YieldRotationConfig): YieldRotationState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.yieldrotation = initState(config, now);
  store.ticks = [{ t: now, price: config.startingPrice }];
  store.events = [
    {
      t: now,
      message: `Yield-rotation started: markets=[${config.markets.join(", ")}], weights(change=${config.wChange}, volume=${config.wVolume}, spread=${config.wSpread}), poll=${config.pollSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.yieldrotation;
}

export function stopYieldRotation(): void {
  const store = getStore();
  stopTimer(store);
  if (store.yieldrotation && store.yieldrotation.status === "monitoring") {
    store.yieldrotation.status = "idle";
    store.events.push({ t: Date.now(), message: "Yield-rotation stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.yieldrotation = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.yieldrotation && store.yieldrotation.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { yieldrotation: store.yieldrotation, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.yieldrotation || store.yieldrotation.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const lastPrice = store.ticks.length > 0 ? store.ticks[store.ticks.length - 1].price : store.yieldrotation.config.startingPrice;
  const price = nextPrice(lastPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.yieldrotation, price, now);
  store.yieldrotation = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
