import { initState, step, type PreTradeCheckConfig, type PreTradeCheckState } from "./pretradecheck-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  pretradecheck: PreTradeCheckState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __pretradecheckDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__pretradecheckDemoStore) {
    g.__pretradecheckDemoStore = {
      pretradecheck: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__pretradecheckDemoStore;
}

export function startPreTradeCheck(config: PreTradeCheckConfig): PreTradeCheckState {
  const store = getStore();
  stopTimer(store);
  store.pretradecheck = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  const activeRules = Object.keys(config.rules).filter(
    (k) => (config.rules as Record<string, unknown>)[k] !== undefined,
  ).length;
  store.events = [
    {
      t: Date.now(),
      message: `pre-trade-check started: ${activeRules} rule(s) active, order arrival λ=${config.orderArrivalPerTick}/tick`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.pretradecheck;
}

export function stopPreTradeCheck(): void {
  const store = getStore();
  stopTimer(store);
  if (store.pretradecheck && store.pretradecheck.status === "monitoring") {
    store.pretradecheck.status = "idle";
    store.events.push({ t: Date.now(), message: "pre-trade-check stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.pretradecheck = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.pretradecheck && store.pretradecheck.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { pretradecheck: store.pretradecheck, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.pretradecheck || store.pretradecheck.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.pretradecheck.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.pretradecheck, price, now);
  store.pretradecheck = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
