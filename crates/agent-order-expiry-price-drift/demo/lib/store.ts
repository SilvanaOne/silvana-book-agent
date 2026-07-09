import { initState, step, type OrderExpiryConfig, type OrderExpiryState } from "./orderexpiry-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  orderexpiry: OrderExpiryState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;

const g = globalThis as unknown as { __orderexpiryDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__orderexpiryDemoStore) {
    g.__orderexpiryDemoStore = {
      orderexpiry: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__orderexpiryDemoStore;
}

export function startOrderExpiry(config: OrderExpiryConfig): OrderExpiryState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.orderexpiry = initState(config, config.startingPrice, now);
  store.ticks = [{ t: now, price: config.startingPrice }];
  store.events = [
    {
      t: now,
      message: `Order Expiry started: market=${config.market}, max-drift=${config.maxDriftPct}%, check every ${config.checkIntervalSecs}s${config.dryRun ? ", DRY-RUN" : ""}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.orderexpiry;
}

export function stopOrderExpiry(): void {
  const store = getStore();
  stopTimer(store);
  if (store.orderexpiry && store.orderexpiry.status === "monitoring") {
    store.orderexpiry.status = "idle";
    store.events.push({ t: Date.now(), message: "Order Expiry stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.orderexpiry = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.orderexpiry && store.orderexpiry.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { orderexpiry: store.orderexpiry, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.orderexpiry || store.orderexpiry.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.orderexpiry.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.orderexpiry, price, now);
  store.orderexpiry = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
