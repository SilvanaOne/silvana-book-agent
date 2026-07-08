import { initState, step, type CashBufferConfig, type CashBufferState } from "./cashbuffer-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; ema: number | null }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  cashbuffer: CashBufferState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
  lastPrice: number;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __cashbufferDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__cashbufferDemoStore) {
    g.__cashbufferDemoStore = {
      cashbuffer: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
      lastPrice: 1,
    };
  }
  return g.__cashbufferDemoStore;
}

export function startCashBuffer(config: CashBufferConfig): CashBufferState {
  const store = getStore();
  stopTimer(store);
  store.cashbuffer = initState(config);
  // Seed a synthetic price track so the price simulator + DemoTools still work.
  const seedPrice = 1;
  store.lastPrice = seedPrice;
  store.ticks = [{ t: Date.now(), price: seedPrice, ema: null }];
  store.events = [
    {
      t: Date.now(),
      message: `Cash Buffer started: band [${config.minCc}, ${config.maxCc}] CC, target ${(config.minCc + config.maxCc) / 2}, sink=${config.sinkParty}, check every ${config.checkIntervalSecs}s, inflow ${config.incomeRate}/tick`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.cashbuffer;
}

export function stopCashBuffer(): void {
  const store = getStore();
  stopTimer(store);
  if (store.cashbuffer && store.cashbuffer.status === "monitoring") {
    store.cashbuffer.status = "idle";
    store.events.push({ t: Date.now(), message: "Cash Buffer stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.cashbuffer = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.cashbuffer && store.cashbuffer.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { cashbuffer: store.cashbuffer, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.cashbuffer || store.cashbuffer.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.lastPrice, store.walk, override);
  store.lastPrice = price;
  const now = Date.now();
  const { state, events } = step(store.cashbuffer, price, now);
  store.cashbuffer = state;
  store.ticks.push({ t: now, price, ema: null });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
