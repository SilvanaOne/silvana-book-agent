import { initState, step, type ObStreamConfig, type ObStreamState } from "./obstream-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  agent: ObStreamState | null;
  driverPrice: number;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;

const g = globalThis as unknown as { __obstreamDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__obstreamDemoStore) {
    g.__obstreamDemoStore = {
      agent: null,
      driverPrice: 1,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.006 },
      priceOverride: null,
    };
  }
  return g.__obstreamDemoStore;
}

export function startAgent(config: ObStreamConfig): ObStreamState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.agent = initState(config, now);
  store.driverPrice = config.startingPrices[0] ?? 1;
  store.ticks = [{ t: now, price: store.driverPrice }];
  const streams = [
    config.noPrices ? null : "prices",
    config.noDepth ? null : "depth",
  ].filter((s): s is string => s !== null);
  store.events = [
    {
      t: now,
      message: `orderbook-streaming started — markets=[${config.markets.join(",")}] streams=[${streams.join(",")}] depth=${config.depth} sinks=${config.sinks.length}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.agent;
}

export function stopAgent(): void {
  const store = getStore();
  stopTimer(store);
  if (store.agent && store.agent.status === "streaming") {
    store.agent.status = "idle";
    store.events.push({ t: Date.now(), message: "orderbook-streaming stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.agent = null;
  store.ticks = [];
  store.events = [];
  store.driverPrice = 1;
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.agent && store.agent.status === "streaming") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { agent: store.agent, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.agent || store.agent.status !== "streaming") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  store.driverPrice = nextPrice(store.driverPrice, store.walk, override);
  const now = Date.now();
  const { state, log } = step(store.agent, store.driverPrice, now);
  store.agent = state;
  store.ticks.push({ t: now, price: store.driverPrice });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of log) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "streaming") stopTimer(store);
}
