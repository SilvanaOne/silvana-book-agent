import { initState, step, type SpotGridConfig, type SpotGridState } from "./spotgrid-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  spotgrid: SpotGridState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __spotgridDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__spotgridDemoStore) {
    g.__spotgridDemoStore = {
      spotgrid: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__spotgridDemoStore;
}

export function startSpotGrid(config: SpotGridConfig): SpotGridState {
  const store = getStore();
  stopTimer(store);
  store.spotgrid = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  const bidRange = config.midPrice * (1 - (config.stepPct / 100) * config.bidLevels);
  const offerRange = config.midPrice * (1 + (config.stepPct / 100) * config.offerLevels);
  store.events = [
    {
      t: Date.now(),
      message: `Spot Grid started: ${config.market}, mid ${config.midPrice}, ${config.bidLevels} bids down to ${round8(bidRange)}, ${config.offerLevels} offers up to ${round8(offerRange)}, step ${config.stepPct}%, qty/level ${config.qtyPerLevel}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.spotgrid;
}

export function stopSpotGrid(): void {
  const store = getStore();
  stopTimer(store);
  if (store.spotgrid && store.spotgrid.status === "monitoring") {
    store.spotgrid.status = "idle";
    store.events.push({ t: Date.now(), message: "Spot Grid stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.spotgrid = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.spotgrid && store.spotgrid.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { spotgrid: store.spotgrid, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.spotgrid || store.spotgrid.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.spotgrid.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.spotgrid, price, now);
  store.spotgrid = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}
