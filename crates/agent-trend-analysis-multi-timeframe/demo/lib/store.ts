import { initState, step, type TrendAnalysisConfig, type TrendAnalysisState, type Alignment } from "./trendanalysis-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{
  t: number;
  price: number;
  smaShort: number | null;
  smaMid: number | null;
  smaLong: number | null;
  alignment: Alignment;
}>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  trendanalysis: TrendAnalysisState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __trendanalysisDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__trendanalysisDemoStore) {
    g.__trendanalysisDemoStore = {
      trendanalysis: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__trendanalysisDemoStore;
}

export function startTrendAnalysis(config: TrendAnalysisConfig): TrendAnalysisState {
  const store = getStore();
  stopTimer(store);
  store.trendanalysis = initState(config, config.startingPrice);
  store.ticks = [
    {
      t: Date.now(),
      price: config.startingPrice,
      smaShort: null,
      smaMid: null,
      smaLong: null,
      alignment: "warmup",
    },
  ];
  store.events = [
    {
      t: Date.now(),
      message:
        `MTF started: ${config.market}, short=${config.short}, mid=${config.mid}, long=${config.long}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.trendanalysis;
}

export function stopTrendAnalysis(): void {
  const store = getStore();
  stopTimer(store);
  if (store.trendanalysis && store.trendanalysis.status === "monitoring") {
    store.trendanalysis.status = "idle";
    store.events.push({ t: Date.now(), message: "MTF stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.trendanalysis = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.trendanalysis && store.trendanalysis.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { trendanalysis: store.trendanalysis, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.trendanalysis || store.trendanalysis.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.trendanalysis.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.trendanalysis, price, now);
  store.trendanalysis = state;
  store.ticks.push({
    t: now,
    price,
    smaShort: state.smaShort,
    smaMid: state.smaMid,
    smaLong: state.smaLong,
    alignment: state.alignment,
  });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
