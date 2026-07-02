import { initState, step, type PnlScreeningConfig, type PnlScreeningState } from "./pnlscreening-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; realized: number; unrealized: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  pnlscreening: PnlScreeningState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __pnlscreeningDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__pnlscreeningDemoStore) {
    g.__pnlscreeningDemoStore = {
      pnlscreening: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__pnlscreeningDemoStore;
}

export function startPnlScreening(config: PnlScreeningConfig): PnlScreeningState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.pnlscreening = initState(config, now);
  store.ticks = [{ t: now, price: config.startingPrice, realized: 0, unrealized: 0 }];
  store.events = [
    {
      t: now,
      message: `PnL Screening started: ${config.market}, snapshot every ${config.snapshotIntervalSecs}s, arrival ${config.tradeArrivalPerTick}/tick, avg qty ${config.avgTradeQty}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.pnlscreening;
}

export function stopPnlScreening(): void {
  const store = getStore();
  stopTimer(store);
  if (store.pnlscreening && store.pnlscreening.status === "monitoring") {
    store.pnlscreening.status = "idle";
    store.events.push({ t: Date.now(), message: "PnL Screening stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.pnlscreening = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.pnlscreening && store.pnlscreening.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { pnlscreening: store.pnlscreening, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.pnlscreening || store.pnlscreening.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.pnlscreening.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.pnlscreening, price, now);
  store.pnlscreening = state;
  store.ticks.push({ t: now, price, realized: state.realizedPnl, unrealized: state.unrealizedPnl });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
