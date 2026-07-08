import { initState, step, type StateMonitorConfig, type StateMonitorState } from "./statemonitor-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  statemonitor: StateMonitorState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __statemonitorDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__statemonitorDemoStore) {
    g.__statemonitorDemoStore = {
      statemonitor: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__statemonitorDemoStore;
}

export function startStateMonitor(config: StateMonitorConfig): StateMonitorState {
  const store = getStore();
  stopTimer(store);
  store.statemonitor = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  const filterLabel = (config.market || "*") === "*" ? "all markets" : config.market;
  const subs: string[] = [];
  if (config.includeOrders) subs.push("orders");
  if (config.includeSettlements) subs.push("settlements");
  store.events = [
    {
      t: Date.now(),
      message: `state-monitor started: ${filterLabel} · subs=[${subs.join(",") || "none"}] · orderArrival=${config.orderArrivalPerTick} · settlementArrival=${config.settlementArrivalPerTick}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.statemonitor;
}

export function stopStateMonitor(): void {
  const store = getStore();
  stopTimer(store);
  if (store.statemonitor && store.statemonitor.status === "monitoring") {
    store.statemonitor.status = "idle";
    store.events.push({ t: Date.now(), message: "state-monitor stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.statemonitor = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.statemonitor && store.statemonitor.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { statemonitor: store.statemonitor, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.statemonitor || store.statemonitor.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.statemonitor.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.statemonitor, price, now);
  store.statemonitor = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
