import { initState, step, type SpreadCaptureConfig, type SpreadCaptureState } from "./spreadcapture-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  spreadcapture: SpreadCaptureState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __spreadcaptureDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__spreadcaptureDemoStore) {
    g.__spreadcaptureDemoStore = {
      spreadcapture: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__spreadcaptureDemoStore;
}

export function startSpreadCapture(config: SpreadCaptureConfig): SpreadCaptureState {
  const store = getStore();
  stopTimer(store);
  store.spreadcapture = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  store.events = [
    {
      t: Date.now(),
      message: `SC multi-level started: ${config.market}, levels ${config.levels}, inner ±${config.innerSpreadBps} bps, step ${config.stepBps} bps, qty/lvl ${config.quantityPerLevel}, max_inv ±${config.maxInventory}, refresh ${config.refreshSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.spreadcapture;
}

export function stopSpreadCapture(): void {
  const store = getStore();
  stopTimer(store);
  if (store.spreadcapture && store.spreadcapture.status === "monitoring") {
    store.spreadcapture.status = "idle";
    store.events.push({ t: Date.now(), message: "SC stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.spreadcapture = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.spreadcapture && store.spreadcapture.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { spreadcapture: store.spreadcapture, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.spreadcapture || store.spreadcapture.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.spreadcapture.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.spreadcapture, price, now);
  store.spreadcapture = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
