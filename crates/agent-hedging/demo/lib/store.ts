import { initState, step, type HedgingConfig, type HedgingState } from "./hedging-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; balance: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  hedging: HedgingState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __hedgingDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__hedgingDemoStore) {
    g.__hedgingDemoStore = {
      hedging: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__hedgingDemoStore;
}

export function startHedging(config: HedgingConfig): HedgingState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.hedging = initState(config, now);
  store.ticks = [{ t: now, price: config.startingPrice, balance: config.startingBalance }];
  store.events = [
    {
      t: now,
      message:
        `Hedging started: instrument=${config.exposureInstrument}, market=${config.hedgeMarket}, ` +
        `target=${config.targetBalance}±${config.tolerance}, fraction=${config.hedgeFraction}, ` +
        `check every ${config.checkIntervalSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.hedging;
}

export function stopHedging(): void {
  const store = getStore();
  stopTimer(store);
  if (store.hedging && store.hedging.status === "monitoring") {
    store.hedging.status = "idle";
    store.events.push({ t: Date.now(), message: "Hedging stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.hedging = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.hedging && store.hedging.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { hedging: store.hedging, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.hedging || store.hedging.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.hedging.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.hedging, price, now);
  store.hedging = state;
  store.ticks.push({ t: now, price, balance: state.currentBalance });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
