// Singleton in-memory store. Next.js keeps route handlers hot in dev,
// so a module-level variable survives across requests. Not suitable for
// production multi-process deploys — perfect for a single-process demo.

import { initState, step, type PositionConfig, type PositionState } from "./tpsl-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  position: PositionState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null; // consumed on next tick
};

const MAX_TICKS = 300;
const MAX_EVENTS = 100;

const g = globalThis as unknown as { __tpslDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__tpslDemoStore) {
    g.__tpslDemoStore = {
      position: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__tpslDemoStore;
}

export function startPosition(config: PositionConfig): PositionState {
  const store = getStore();
  stopTimer(store);
  store.position = initState(config, config.entryPrice);
  store.ticks = [{ t: Date.now(), price: config.entryPrice }];
  store.events = [{ t: Date.now(), message: `Position opened: ${config.side.toUpperCase()} ${config.quantity} ${config.market} @ ${config.entryPrice}` }];
  store.priceOverride = null;
  startTimer(store);
  return store.position;
}

export function stopPosition(): void {
  const store = getStore();
  stopTimer(store);
  if (store.position && store.position.status === "monitoring") {
    store.position.status = "idle";
    store.events.push({ t: Date.now(), message: "Monitor stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.position = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.position && store.position.status === "monitoring") {
    store.priceOverride = to;
  }
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return {
    position: store.position,
    ticks: store.ticks,
    events: store.events,
    walk: store.walk,
  };
}

// ---------------------------------------------------------------- internals
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
  if (!store.position || store.position.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.position.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.position, price, now);
  store.position = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) {
    store.events.push({ t: now, message: e });
  }
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status === "triggered") {
    stopTimer(store);
  }
}
