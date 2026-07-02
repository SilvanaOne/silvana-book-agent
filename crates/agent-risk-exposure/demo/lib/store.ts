import { initState, step, type RiskExposureConfig, type RiskExposureState } from "./riskexposure-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; totalPortfolioQuote: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  riskexposure: RiskExposureState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __riskexposureDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__riskexposureDemoStore) {
    g.__riskexposureDemoStore = {
      riskexposure: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__riskexposureDemoStore;
}

export function startRiskExposure(config: RiskExposureConfig): RiskExposureState {
  const store = getStore();
  stopTimer(store);
  store.riskexposure = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, totalPortfolioQuote: store.riskexposure.totalPortfolioQuote }];
  const instList = config.instruments.map((i) => `${i.name}:${i.balance}`).join(",");
  store.events = [
    {
      t: Date.now(),
      message: `Risk-Exposure started: markets=[${config.markets.join(",")}] instruments=[${instList}] warn>${config.concentrationWarnPct}% interval=${config.snapshotIntervalSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.riskexposure;
}

export function stopRiskExposure(): void {
  const store = getStore();
  stopTimer(store);
  if (store.riskexposure && store.riskexposure.status === "monitoring") {
    store.riskexposure.status = "idle";
    store.events.push({ t: Date.now(), message: "Risk-Exposure stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.riskexposure = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.riskexposure && store.riskexposure.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { riskexposure: store.riskexposure, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.riskexposure || store.riskexposure.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.riskexposure.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.riskexposure, price, now);
  store.riskexposure = state;
  store.ticks.push({ t: now, price, totalPortfolioQuote: state.totalPortfolioQuote });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
