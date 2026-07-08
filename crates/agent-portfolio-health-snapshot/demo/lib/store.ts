import { initState, step, type PortfolioHealthConfig, type PortfolioHealthState } from "./portfoliohealth-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; portfolioValue: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  portfoliohealth: PortfolioHealthState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __portfoliohealthDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__portfoliohealthDemoStore) {
    g.__portfoliohealthDemoStore = {
      portfoliohealth: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__portfoliohealthDemoStore;
}

export function startPortfolioHealth(config: PortfolioHealthConfig): PortfolioHealthState {
  const store = getStore();
  stopTimer(store);
  store.portfoliohealth = initState(config);
  const now = Date.now();
  store.ticks = [{ t: now, price: config.startingPrice, portfolioValue: store.portfoliohealth.portfolioValueQuote }];
  store.events = [
    {
      t: now,
      message: `Portfolio Health started: markets=[${config.markets.join(",")}] instruments=${config.instruments.length} snapshot=${config.snapshotIntervalSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.portfoliohealth;
}

export function stopPortfolioHealth(): void {
  const store = getStore();
  stopTimer(store);
  if (store.portfoliohealth && store.portfoliohealth.status === "monitoring") {
    store.portfoliohealth.status = "idle";
    store.events.push({ t: Date.now(), message: "Portfolio Health stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.portfoliohealth = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.portfoliohealth && store.portfoliohealth.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { portfoliohealth: store.portfoliohealth, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.portfoliohealth || store.portfoliohealth.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.portfoliohealth.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.portfoliohealth, price, now);
  store.portfoliohealth = state;
  store.ticks.push({ t: now, price, portfolioValue: state.portfolioValueQuote });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
