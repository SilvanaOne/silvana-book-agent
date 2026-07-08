import { initState, step, type OrderMatchingConfig, type OrderMatchingState } from "./ordermatching-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{
  t: number;
  price: number;
  bestBid: number | null;
  bestOffer: number | null;
}>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  ordermatching: OrderMatchingState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __ordermatchingDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__ordermatchingDemoStore) {
    g.__ordermatchingDemoStore = {
      ordermatching: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__ordermatchingDemoStore;
}

export function startOrderMatching(config: OrderMatchingConfig): OrderMatchingState {
  const store = getStore();
  stopTimer(store);
  store.ordermatching = initState(config, config.startingPrice);
  const st = store.ordermatching;
  store.ticks = [{ t: Date.now(), price: config.startingPrice, bestBid: st.bestBid, bestOffer: st.bestOffer }];
  const trigParts: string[] = [];
  if (config.buyTrigger !== null) trigParts.push(`buy≤${config.buyTrigger}`);
  if (config.sellTrigger !== null) trigParts.push(`sell≥${config.sellTrigger}`);
  store.events = [
    {
      t: Date.now(),
      message: `Order-matching started: ${config.market}, ${trigParts.join(" / ") || "no triggers"}, qty ${config.quantity}, book spread ${config.bookSpreadBps} bps`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.ordermatching;
}

export function stopOrderMatching(): void {
  const store = getStore();
  stopTimer(store);
  if (store.ordermatching && store.ordermatching.status === "monitoring") {
    store.ordermatching.status = "idle";
    store.events.push({ t: Date.now(), message: "Order-matching stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.ordermatching = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.ordermatching && store.ordermatching.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { ordermatching: store.ordermatching, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.ordermatching || store.ordermatching.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.ordermatching.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.ordermatching, price, now);
  store.ordermatching = state;
  store.ticks.push({ t: now, price, bestBid: state.bestBid, bestOffer: state.bestOffer });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
