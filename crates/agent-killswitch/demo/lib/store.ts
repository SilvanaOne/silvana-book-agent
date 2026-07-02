import { initState, step, panic, type KillswitchConfig, type KillswitchState } from "./killswitch-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

// Tick shape is unchanged for compatibility with the chart's time axis.
// We reuse the `price` field to carry the "open orders" series and
// `ema` to carry the "failed settlements" series so the shared price
// snapshot API keeps working, but the chart interprets them by name.
export type Tick = Readonly<{ t: number; price: number; ema: number | null }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  killswitch: KillswitchState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __killswitchDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__killswitchDemoStore) {
    g.__killswitchDemoStore = {
      killswitch: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__killswitchDemoStore;
}

export function startKillswitch(config: KillswitchConfig): KillswitchState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.killswitch = initState(config, now);
  store.ticks = [{ t: now, price: store.killswitch.openOrders, ema: store.killswitch.failedSettlements }];
  store.events = [
    {
      t: now,
      message: `Killswitch monitor started: interval=${config.checkIntervalSecs}s max_failed=${config.maxFailedSettlements} max_open=${config.maxOpenOrders} dry_run=false`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.killswitch;
}

export function stopKillswitch(): void {
  const store = getStore();
  stopTimer(store);
  if (store.killswitch && store.killswitch.status === "monitoring") {
    store.killswitch.status = "idle";
    store.events.push({ t: Date.now(), message: "Killswitch monitor stopped by operator" });
  }
}

export function panicKillswitch(): void {
  const store = getStore();
  if (!store.killswitch) return;
  const now = Date.now();
  const { state, events } = panic(store.killswitch, now);
  store.killswitch = state;
  for (const e of events) store.events.push({ t: now, message: e });
  // Freeze the last tick with the final snapshot.
  store.ticks.push({ t: now, price: state.openOrders, ema: state.failedSettlements });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  stopTimer(store);
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.killswitch = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.killswitch && store.killswitch.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { killswitch: store.killswitch, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.killswitch || store.killswitch.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  // Keep price sim running so demo tools stay wired, but the value is unused
  // by the killswitch logic. We still surface it on state.currentPrice for the
  // shared DemoTools panel.
  const _p = nextPrice(store.killswitch.currentPrice || 1, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.killswitch, _p, now);
  state.currentPrice = _p;
  store.killswitch = state;
  store.ticks.push({ t: now, price: state.openOrders, ema: state.failedSettlements });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
