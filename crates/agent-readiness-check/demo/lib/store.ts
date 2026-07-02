import { initState, step, type ReadinessCheckConfig, type ReadinessCheckState } from "./readinesscheck-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; ready: boolean | null }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  readinesscheck: ReadinessCheckState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __readinesscheckDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__readinesscheckDemoStore) {
    g.__readinesscheckDemoStore = {
      readinesscheck: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__readinesscheckDemoStore;
}

export function startReadinessCheck(config: ReadinessCheckConfig): ReadinessCheckState {
  const store = getStore();
  stopTimer(store);
  store.readinesscheck = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, ready: null }];
  const reqStr = config.requiredBalances.map((r) => `${r.instrument}>=${r.minAmount}`).join(", ");
  store.events = [
    {
      t: Date.now(),
      message: `Readiness check started: required=[${reqStr}], maxFailed=${config.maxFailedSettlements}, maxPending=${config.maxPendingSettlements}, preapproval=${config.requirePreapproval}, interval=${config.checkIntervalSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.readinesscheck;
}

export function stopReadinessCheck(): void {
  const store = getStore();
  stopTimer(store);
  if (store.readinesscheck && store.readinesscheck.status === "monitoring") {
    store.readinesscheck.status = "idle";
    store.events.push({ t: Date.now(), message: "Readiness check stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.readinesscheck = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.readinesscheck && store.readinesscheck.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { readinesscheck: store.readinesscheck, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.readinesscheck || store.readinesscheck.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.readinesscheck.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.readinesscheck, price, now);
  store.readinesscheck = state;
  const ready = state.checksCount === 0 ? null : state.overallReady;
  store.ticks.push({ t: now, price, ready });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
