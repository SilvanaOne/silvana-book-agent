import { initState, step, type BlockExecutionConfig, type BlockExecutionState } from "./blockexecution-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  blockexecution: BlockExecutionState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;

const g = globalThis as unknown as { __blockexecutionDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__blockexecutionDemoStore) {
    g.__blockexecutionDemoStore = {
      blockexecution: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__blockexecutionDemoStore;
}

export function startBlockExecution(config: BlockExecutionConfig): BlockExecutionState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.blockexecution = initState(config, config.startingPrice, now);
  store.ticks = [{ t: now, price: config.startingPrice }];
  store.events = [
    {
      t: now,
      message: `Block Execution started: ${config.market} ${config.side} total=${config.total} @ ${config.price} slices=${config.timeSlices} duration=${config.durationSecs}s visible=${config.visible}`,
    },
    {
      t: now,
      message: `SLICE 1/${config.timeSlices} start: target=${(config.total / config.timeSlices).toFixed(6)} window=${(config.durationSecs / config.timeSlices).toFixed(1)}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.blockexecution;
}

export function stopBlockExecution(): void {
  const store = getStore();
  stopTimer(store);
  if (store.blockexecution && store.blockexecution.status === "monitoring") {
    store.blockexecution.status = "idle";
    store.events.push({ t: Date.now(), message: "Block Execution stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.blockexecution = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.blockexecution && store.blockexecution.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { blockexecution: store.blockexecution, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.blockexecution || store.blockexecution.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.blockexecution.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.blockexecution, price, now);
  store.blockexecution = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
