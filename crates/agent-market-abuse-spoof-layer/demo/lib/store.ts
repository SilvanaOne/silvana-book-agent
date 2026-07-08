import { initState, step, type MarketAbuseConfig, type MarketAbuseState } from "./marketabuse-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  marketabuse: MarketAbuseState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __marketabuseDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__marketabuseDemoStore) {
    g.__marketabuseDemoStore = {
      marketabuse: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__marketabuseDemoStore;
}

export function startMarketAbuse(config: MarketAbuseConfig): MarketAbuseState {
  const store = getStore();
  stopTimer(store);
  store.marketabuse = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  store.events = [
    {
      t: Date.now(),
      message: `Market-abuse monitor started: ${config.market}, spoof≥${config.spoofBurst}/${config.spoofBurstWindowSecs}s, layer≥${config.layerMinOrders} in ${config.layerPriceBandPct}%`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.marketabuse;
}

export function stopMarketAbuse(): void {
  const store = getStore();
  stopTimer(store);
  if (store.marketabuse && store.marketabuse.status === "monitoring") {
    store.marketabuse.status = "idle";
    store.events.push({ t: Date.now(), message: "Market-abuse monitor stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.marketabuse = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.marketabuse && store.marketabuse.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { marketabuse: store.marketabuse, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.marketabuse || store.marketabuse.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.marketabuse.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.marketabuse, price, now);
  store.marketabuse = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
