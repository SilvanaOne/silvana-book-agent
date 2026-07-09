import { initState, step, type TwapConfig, type TwapState } from "./twap-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  twap: TwapState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __twapVwapDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__twapVwapDemoStore) {
    g.__twapVwapDemoStore = {
      twap: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__twapVwapDemoStore;
}

export function startTwap(config: TwapConfig): TwapState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.twap = initState(config, config.startingPrice, now);
  store.ticks = [{ t: now, price: config.startingPrice }];
  const lim = config.limitPrice === null ? "none" : String(config.limitPrice);
  const curveDisplay = config.volumeCurve
    .map((w) => `${(w * 100).toFixed(1)}%`)
    .join(",");
  store.events = [
    {
      t: now,
      message: `VWAP started: ${config.market} ${config.side.toUpperCase()} total=${config.total} slices=${config.slices} duration=${config.durationSecs}s curve=\`${config.volumeCurveSpec}\` weights=[${curveDisplay}] offset=${config.priceOffsetPct}% limit=${lim}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.twap;
}

export function stopTwap(): void {
  const store = getStore();
  stopTimer(store);
  if (store.twap && store.twap.status === "monitoring") {
    store.twap.status = "idle";
    store.events.push({ t: Date.now(), message: "VWAP stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.twap = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.twap && store.twap.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { twap: store.twap, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.twap || store.twap.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.twap.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.twap, price, now);
  store.twap = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
