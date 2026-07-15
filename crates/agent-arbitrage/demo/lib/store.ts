import {
  initState,
  step,
  type ArbitrageConfig,
  type ArbitrageState,
} from "./arbitrage-engine";
import type { WalkParams } from "./spread-simulator";

export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  arbitrage: ArbitrageState | null;
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  bpsOverride: number | null; // one-shot override for the focus pair's bps
};

const MAX_EVENTS = 120;

const g = globalThis as unknown as { __arbitrageDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__arbitrageDemoStore) {
    g.__arbitrageDemoStore = {
      arbitrage: null,
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 6 },
      bpsOverride: null,
    };
  }
  return g.__arbitrageDemoStore;
}

export function startArbitrage(config: ArbitrageConfig): ArbitrageState {
  const store = getStore();
  stopTimer(store);
  store.arbitrage = initState(config);
  store.events = [
    {
      t: Date.now(),
      message: `Scanner started: focus ${config.focusPair}, act ≥ ${config.minSpreadBps} bps, size $${config.tradeSizeUsd}, scan ${config.scanIntervalSecs}s`,
    },
  ];
  store.bpsOverride = null;
  startTimer(store);
  return store.arbitrage;
}

export function stopArbitrage(): void {
  const store = getStore();
  stopTimer(store);
  if (store.arbitrage && store.arbitrage.status === "scanning") {
    store.arbitrage.status = "idle";
    store.events.push({ t: Date.now(), message: "Scanner stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.arbitrage = null;
  store.events = [];
  store.bpsOverride = null;
}

export function nudgeSpread(toBps: number): void {
  const store = getStore();
  if (store.arbitrage && store.arbitrage.status === "scanning") store.bpsOverride = toBps;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { arbitrage: store.arbitrage, events: store.events, walk: store.walk };
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
  if (!store.arbitrage || store.arbitrage.status !== "scanning") {
    stopTimer(store);
    return;
  }
  const override = store.bpsOverride;
  store.bpsOverride = null;
  const now = Date.now();
  const { state, events } = step(store.arbitrage, store.walk, now, override);
  store.arbitrage = state;
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "scanning") stopTimer(store);
}
