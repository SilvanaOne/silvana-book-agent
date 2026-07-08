import { initState, step, type IcebergExecutionConfig, type IcebergExecutionState } from "./icebergexecution-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  icebergexecution: IcebergExecutionState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __icebergexecutionDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__icebergexecutionDemoStore) {
    g.__icebergexecutionDemoStore = {
      icebergexecution: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__icebergexecutionDemoStore;
}

export function startIcebergExecution(config: IcebergExecutionConfig): IcebergExecutionState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.icebergexecution = initState(config, config.startingPrice, now);
  store.ticks = [{ t: now, price: config.startingPrice }];
  store.events = [
    {
      t: now,
      message: `Iceberg started: ${config.market} ${config.side} total=${config.total} visible=${config.visible} price=${config.price} maxRuntime=${config.maxRuntimeSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.icebergexecution;
}

export function stopIcebergExecution(): void {
  const store = getStore();
  stopTimer(store);
  if (store.icebergexecution && store.icebergexecution.status === "monitoring") {
    store.icebergexecution.status = "idle";
    store.events.push({ t: Date.now(), message: "Iceberg stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.icebergexecution = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.icebergexecution && store.icebergexecution.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { icebergexecution: store.icebergexecution, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.icebergexecution || store.icebergexecution.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.icebergexecution.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.icebergexecution, price, now);
  store.icebergexecution = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
