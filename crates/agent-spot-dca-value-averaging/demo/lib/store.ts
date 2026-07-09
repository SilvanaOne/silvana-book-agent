import { initState, step, type DcaConfig, type DcaState } from "./dca-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  dca: DcaState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __dcaDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__dcaDemoStore) {
    g.__dcaDemoStore = {
      dca: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.005 },
      priceOverride: null,
    };
  }
  return g.__dcaDemoStore;
}

export function startDca(config: DcaConfig): DcaState {
  const store = getStore();
  stopTimer(store);
  store.dca = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  store.events = [{ t: Date.now(), message: `VA DCA started: ${config.side.toUpperCase()} target ${config.valuePerPeriod} quote/period on ${config.market} every ${config.intervalSecs}s at mid ${config.priceOffsetPct >= 0 ? "+" : ""}${config.priceOffsetPct}%` }];
  store.priceOverride = null;
  startTimer(store);
  return store.dca;
}

export function stopDca(): void {
  const store = getStore();
  stopTimer(store);
  if (store.dca && store.dca.status === "monitoring") {
    store.dca.status = "idle";
    store.events.push({ t: Date.now(), message: "DCA stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.dca = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.dca && store.dca.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { dca: store.dca, ticks: store.ticks, events: store.events, walk: store.walk };
}

function startTimer(store: StoreState) {
  store.timer = setInterval(() => tick(store), 1000);
}
function stopTimer(store: StoreState) {
  if (store.timer) { clearInterval(store.timer); store.timer = null; }
}
function tick(store: StoreState) {
  if (!store.dca || store.dca.status !== "monitoring") { stopTimer(store); return; }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.dca.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.dca, price, now);
  store.dca = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
