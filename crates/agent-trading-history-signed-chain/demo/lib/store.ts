import { initState, step, tamperRecord, type TradingHistoryConfig, type TradingHistoryState } from "./tradinghistory-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  tradinghistory: TradingHistoryState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __tradinghistoryDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__tradinghistoryDemoStore) {
    g.__tradinghistoryDemoStore = {
      tradinghistory: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__tradinghistoryDemoStore;
}

export function startTradingHistory(config: TradingHistoryConfig): TradingHistoryState {
  const store = getStore();
  stopTimer(store);
  store.tradinghistory = initState(config);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  const mkt = config.market ? `market=${config.market}` : "market=* (all)";
  const streams = [config.orders ? "orders" : null, config.settlements ? "settlements" : null].filter(Boolean).join("+");
  store.events = [
    { t: Date.now(), message: `TH started: streams=${streams}, ${mkt}, arrival λ=${config.eventArrivalPerTick}/tick` },
    { t: Date.now(), message: `GENESIS hash seeded — chain empty` },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.tradinghistory;
}

export function stopTradingHistory(): void {
  const store = getStore();
  stopTimer(store);
  if (store.tradinghistory && store.tradinghistory.status === "monitoring") {
    store.tradinghistory.status = "idle";
    store.events.push({ t: Date.now(), message: "TH stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.tradinghistory = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.tradinghistory && store.tradinghistory.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function tamperAt(index: number): { ok: boolean; message: string } {
  const store = getStore();
  if (!store.tradinghistory) return { ok: false, message: "not running" };
  const ok = tamperRecord(store.tradinghistory, index);
  if (!ok) return { ok: false, message: `index ${index} out of range` };
  const rec = store.tradinghistory.records[index];
  store.events.push({ t: Date.now(), message: `TAMPER: record #${rec.seq} (${rec.kind}) payload mutated — chain broken from index ${index}` });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  return { ok: true, message: `tampered #${rec.seq}` };
}

export function snapshot() {
  const store = getStore();
  return { tradinghistory: store.tradinghistory, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.tradinghistory || store.tradinghistory.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.tradinghistory.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.tradinghistory, price, now);
  store.tradinghistory = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
