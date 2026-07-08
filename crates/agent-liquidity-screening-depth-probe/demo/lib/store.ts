import { initState, step, type LiquidityScreeningConfig, type LiquidityScreeningState } from "./liquidityscreening-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; spreadBps: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  liquidityscreening: LiquidityScreeningState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __liquidityscreeningDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__liquidityscreeningDemoStore) {
    g.__liquidityscreeningDemoStore = {
      liquidityscreening: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__liquidityscreeningDemoStore;
}

export function startLiquidityScreening(config: LiquidityScreeningConfig): LiquidityScreeningState {
  const store = getStore();
  stopTimer(store);
  store.liquidityscreening = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, spreadBps: store.liquidityscreening.spreadBps }];
  store.events = [
    {
      t: Date.now(),
      message: `LIQ started: ${config.market}, probe ${config.probeQty}, depth ${config.depth}, poll ${config.pollSecs}s, spread ${config.spreadBps} bps`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.liquidityscreening;
}

export function stopLiquidityScreening(): void {
  const store = getStore();
  stopTimer(store);
  if (store.liquidityscreening && store.liquidityscreening.status === "monitoring") {
    store.liquidityscreening.status = "idle";
    store.events.push({ t: Date.now(), message: "LIQ stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.liquidityscreening = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.liquidityscreening && store.liquidityscreening.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { liquidityscreening: store.liquidityscreening, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.liquidityscreening || store.liquidityscreening.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.liquidityscreening.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.liquidityscreening, price, now);
  store.liquidityscreening = state;
  store.ticks.push({ t: now, price, spreadBps: state.spreadBps });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
