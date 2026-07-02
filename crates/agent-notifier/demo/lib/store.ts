import { initState, step, type NotifierConfig, type NotifierState } from "./notifier-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  notifier: NotifierState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 160;

const g = globalThis as unknown as { __notifierDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__notifierDemoStore) {
    g.__notifierDemoStore = {
      notifier: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__notifierDemoStore;
}

export function startNotifier(config: NotifierConfig): NotifierState {
  const store = getStore();
  stopTimer(store);
  store.notifier = initState(config);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  const streams = [
    config.orders ? "orders" : null,
    config.settlements ? "settlements" : null,
    config.prices ? "prices" : null,
  ].filter((s): s is string => s !== null);
  store.events = [
    {
      t: Date.now(),
      message: `Notifier started — streams=[${streams.join(",")}] sinks=${config.sinks.length}${config.market ? ` filter=${config.market}` : ""}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.notifier;
}

export function stopNotifier(): void {
  const store = getStore();
  stopTimer(store);
  if (store.notifier && store.notifier.status === "monitoring") {
    store.notifier.status = "idle";
    store.events.push({ t: Date.now(), message: "Notifier stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.notifier = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.notifier && store.notifier.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { notifier: store.notifier, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.notifier || store.notifier.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.notifier.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, log } = step(store.notifier, price, now);
  store.notifier = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of log) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
