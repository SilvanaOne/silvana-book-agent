import { initState, step, type PortfolioRebalancingConfig, type PortfolioRebalancingState } from "./portfoliorebalancing-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; portfolioValueQuote: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  portfoliorebalancing: PortfolioRebalancingState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __portfoliorebalancingDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__portfoliorebalancingDemoStore) {
    g.__portfoliorebalancingDemoStore = {
      portfoliorebalancing: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__portfoliorebalancingDemoStore;
}

export function startPortfolioRebalancing(config: PortfolioRebalancingConfig): PortfolioRebalancingState {
  const store = getStore();
  stopTimer(store);
  store.portfoliorebalancing = initState(config);
  const s = store.portfoliorebalancing;
  store.ticks = [{ t: Date.now(), price: config.startingPrice, portfolioValueQuote: s.portfolioValueQuote }];
  store.events = [
    {
      t: Date.now(),
      message:
        `Rebalancing started: ${config.targets.length} targets, ` +
        `bands +${config.upperBandPct}pp / -${config.lowerBandPct}pp, ` +
        `offset ${config.priceOffsetPct}%, cycle ${config.checkIntervalSecs}s (event-driven single-shot back-to-target)`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.portfoliorebalancing;
}

export function stopPortfolioRebalancing(): void {
  const store = getStore();
  stopTimer(store);
  if (store.portfoliorebalancing && store.portfoliorebalancing.status === "rebalancing") {
    store.portfoliorebalancing.status = "idle";
    store.events.push({ t: Date.now(), message: "Rebalancing stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.portfoliorebalancing = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.portfoliorebalancing && store.portfoliorebalancing.status === "rebalancing") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { portfoliorebalancing: store.portfoliorebalancing, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.portfoliorebalancing || store.portfoliorebalancing.status !== "rebalancing") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.portfoliorebalancing.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.portfoliorebalancing, price, now);
  store.portfoliorebalancing = state;
  store.ticks.push({ t: now, price, portfolioValueQuote: state.portfolioValueQuote });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "rebalancing") stopTimer(store);
}
