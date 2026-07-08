import { initState, step, type RiskAlertConfig, type RiskAlertState } from "./riskalert-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{
  t: number;
  price: number;
  openOrders: number;
  failedSettlements: number;
  openNotional: number;
}>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  riskalert: RiskAlertState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __riskalertDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__riskalertDemoStore) {
    g.__riskalertDemoStore = {
      riskalert: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__riskalertDemoStore;
}

export function startRiskAlert(config: RiskAlertConfig): RiskAlertState {
  const store = getStore();
  stopTimer(store);
  store.riskalert = initState(config);
  store.ticks = [
    { t: Date.now(), price: config.startingPrice, openOrders: 0, failedSettlements: 0, openNotional: 0 },
  ];
  store.events = [
    {
      t: Date.now(),
      message: `risk-alert armed: open<=${config.maxOpenOrders}, failed<=${config.maxFailedSettlements}, notional<=${config.maxOpenNotional}, check every ${config.checkIntervalSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.riskalert;
}

export function stopRiskAlert(): void {
  const store = getStore();
  stopTimer(store);
  if (store.riskalert && store.riskalert.status === "monitoring") {
    store.riskalert.status = "idle";
    store.events.push({ t: Date.now(), message: "risk-alert stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.riskalert = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.riskalert && store.riskalert.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { riskalert: store.riskalert, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.riskalert || store.riskalert.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.riskalert.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.riskalert, price, now);
  store.riskalert = state;
  store.ticks.push({
    t: now,
    price,
    openOrders: state.openOrders,
    failedSettlements: state.failedSettlements,
    openNotional: state.openNotional,
  });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
