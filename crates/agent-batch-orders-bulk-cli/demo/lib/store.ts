import { initState, step, type BatchOrdersConfig, type BatchOrdersState } from "./batchorders-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  batchorders: BatchOrdersState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;

const g = globalThis as unknown as { __batchordersDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__batchordersDemoStore) {
    g.__batchordersDemoStore = {
      batchorders: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__batchordersDemoStore;
}

export function startBatchOrders(config: BatchOrdersConfig): BatchOrdersState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.batchorders = initState(config, config.startingPrice, now);
  store.ticks = [{ t: now, price: config.startingPrice }];
  store.events = [
    {
      t: now,
      message: `batch started — ${config.orders.length} order(s), submit-rate ${config.submitRatePerTick}/tick, abort-on-error=${config.abortOnError}, failure-rate=${(config.failureRatePerTick * 100).toFixed(1)}%`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.batchorders;
}

export function stopBatchOrders(): void {
  const store = getStore();
  stopTimer(store);
  if (
    store.batchorders &&
    (store.batchorders.status === "submitting" || store.batchorders.status === "monitoring")
  ) {
    store.batchorders.status = "cancelled";
    store.events.push({ t: Date.now(), message: "batch stopped by operator" });
  }
}

export function cancelAllBatchOrders(): void {
  const store = getStore();
  if (!store.batchorders) return;
  if (
    store.batchorders.status !== "submitting" &&
    store.batchorders.status !== "monitoring"
  ) {
    return;
  }
  store.batchorders.cancelSignal = true;
  store.events.push({ t: Date.now(), message: "cancel-all signal enqueued" });
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.batchorders = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (
    store.batchorders &&
    (store.batchorders.status === "submitting" || store.batchorders.status === "monitoring")
  ) {
    store.priceOverride = to;
  }
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { batchorders: store.batchorders, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.batchorders) {
    stopTimer(store);
    return;
  }
  const s = store.batchorders.status;
  if (s === "completed" || s === "cancelled" || s === "idle") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.batchorders.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.batchorders, price, now);
  store.batchorders = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (
    state.status === "completed" ||
    state.status === "cancelled" ||
    state.status === "idle"
  ) {
    stopTimer(store);
  }
}
