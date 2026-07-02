import { initState, step, type FailureRecoveryConfig, type FailureRecoveryState } from "./failurerecovery-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  failurerecovery: FailureRecoveryState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;

const g = globalThis as unknown as { __failurerecoveryDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__failurerecoveryDemoStore) {
    g.__failurerecoveryDemoStore = {
      failurerecovery: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__failurerecoveryDemoStore;
}

export function startFailureRecovery(config: FailureRecoveryConfig): FailureRecoveryState {
  const store = getStore();
  stopTimer(store);
  store.failurerecovery = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  store.events = [
    {
      t: Date.now(),
      message: `failure-recovery started: maxAge=${config.maxPendingAgeSecs}s, interval=${config.checkIntervalSecs}s, cancel-related=${config.cancelRelatedOrders}, dry-run=${config.dryRun}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.failurerecovery;
}

export function stopFailureRecovery(): void {
  const store = getStore();
  stopTimer(store);
  if (store.failurerecovery && store.failurerecovery.status === "monitoring") {
    store.failurerecovery.status = "idle";
    store.events.push({ t: Date.now(), message: "failure-recovery stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.failurerecovery = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.failurerecovery && store.failurerecovery.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { failurerecovery: store.failurerecovery, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.failurerecovery || store.failurerecovery.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.failurerecovery.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.failurerecovery, price, now);
  store.failurerecovery = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
