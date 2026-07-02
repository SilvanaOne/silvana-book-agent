import { initState, step, type ConcentrationRiskConfig, type ConcentrationRiskState } from "./concentrationrisk-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; totalPortfolio: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  concentrationrisk: ConcentrationRiskState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __concentrationriskDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__concentrationriskDemoStore) {
    g.__concentrationriskDemoStore = {
      concentrationrisk: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__concentrationriskDemoStore;
}

export function startConcentrationRisk(config: ConcentrationRiskConfig): ConcentrationRiskState {
  const store = getStore();
  stopTimer(store);
  store.concentrationrisk = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, totalPortfolio: store.concentrationrisk.totalPortfolio }];
  const names = config.instruments.map((i) => `${i.name}@${i.market}`).join(", ");
  store.events = [
    {
      t: Date.now(),
      message: `Concentration Risk started: ${config.instruments.length} instruments [${names}], max ${config.maxSharePct}% / min ${config.minSharePct}%, check every ${config.checkIntervalSecs}s${config.dryRun ? " (dry-run)" : ""}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.concentrationrisk;
}

export function stopConcentrationRisk(): void {
  const store = getStore();
  stopTimer(store);
  if (store.concentrationrisk && store.concentrationrisk.status === "monitoring") {
    store.concentrationrisk.status = "idle";
    store.events.push({ t: Date.now(), message: "Concentration Risk stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.concentrationrisk = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.concentrationrisk && store.concentrationrisk.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { concentrationrisk: store.concentrationrisk, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.concentrationrisk || store.concentrationrisk.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.concentrationrisk.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.concentrationrisk, price, now);
  store.concentrationrisk = state;
  store.ticks.push({ t: now, price, totalPortfolio: state.totalPortfolio });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
