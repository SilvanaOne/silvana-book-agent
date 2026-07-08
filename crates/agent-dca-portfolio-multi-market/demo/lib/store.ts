import { initState, step, type DcaPortfolioConfig, type DcaPortfolioState } from "./dcaportfolio-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; prices: Record<string, number> }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  dcaportfolio: DcaPortfolioState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  // per-market price overrides applied on the next tick, then cleared
  priceOverrides: Record<string, number>;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __dcaportfolioDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__dcaportfolioDemoStore) {
    g.__dcaportfolioDemoStore = {
      dcaportfolio: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverrides: {},
    };
  }
  return g.__dcaportfolioDemoStore;
}

export function startDcaPortfolio(config: DcaPortfolioConfig): DcaPortfolioState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.dcaportfolio = initState(config, now);
  const initialPrices: Record<string, number> = {};
  for (const mp of store.dcaportfolio.progress) initialPrices[mp.market] = mp.currentPrice;
  store.ticks = [{ t: now, prices: initialPrices }];
  store.events = [
    {
      t: now,
      message: `DCA-PORTFOLIO started: markets=[${config.markets.join(", ")}] side=${config.side} amount=${config.amountPerOrder} interval=${config.intervalSecs}s offset=${config.priceOffsetPct}%${config.maxTotal !== null ? ` maxTotal=${config.maxTotal}/market` : ""}`,
    },
  ];
  store.priceOverrides = {};
  startTimer(store);
  return store.dcaportfolio;
}

export function stopDcaPortfolio(): void {
  const store = getStore();
  stopTimer(store);
  if (store.dcaportfolio && store.dcaportfolio.status === "running") {
    store.dcaportfolio.status = "idle";
    store.events.push({ t: Date.now(), message: "DCA-PORTFOLIO stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.dcaportfolio = null;
  store.ticks = [];
  store.events = [];
  store.priceOverrides = {};
}

export function jumpPrice(market: string | undefined, to: number): void {
  const store = getStore();
  if (!store.dcaportfolio || store.dcaportfolio.status !== "running") return;
  // If no market specified, apply to first market (backwards compat with demo tools)
  const target = market ?? store.dcaportfolio.progress[0]?.market;
  if (!target) return;
  store.priceOverrides[target] = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { dcaportfolio: store.dcaportfolio, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.dcaportfolio || store.dcaportfolio.status !== "running") {
    stopTimer(store);
    return;
  }
  const now = Date.now();
  // Advance each market's internal price via GBM (with optional per-market override)
  const prices: Record<string, number> = {};
  for (const mp of store.dcaportfolio.progress) {
    const override = store.priceOverrides[mp.market];
    const next = nextPrice(mp.currentPrice, store.walk, override);
    mp.currentPrice = next;
    prices[mp.market] = next;
  }
  store.priceOverrides = {};

  const { state, events } = step(store.dcaportfolio, 0, now);
  store.dcaportfolio = state;
  store.ticks.push({ t: now, prices });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "running") stopTimer(store);
}
