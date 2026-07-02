import { initState, step, type TargetAllocationConfig, type TargetAllocationState } from "./targetallocation-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; positions: { instrument: string; currentQuote: number; targetQuote: number }[] }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  targetallocation: TargetAllocationState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __targetallocationDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__targetallocationDemoStore) {
    g.__targetallocationDemoStore = {
      targetallocation: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__targetallocationDemoStore;
}

function snapPositions(s: TargetAllocationState): { instrument: string; currentQuote: number; targetQuote: number }[] {
  return s.positions.map((p) => ({ instrument: p.instrument, currentQuote: p.currentQuote, targetQuote: p.targetQuote }));
}

export function startTargetAllocation(config: TargetAllocationConfig): TargetAllocationState {
  const store = getStore();
  stopTimer(store);
  store.targetallocation = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, positions: snapPositions(store.targetallocation) }];
  const targetSummary = config.targets.map((t) => `${t.instrument}@${t.market}=${t.targetQuote}`).join(", ");
  store.events = [
    {
      t: Date.now(),
      message: `Target allocation started: ${targetSummary} · threshold=${config.thresholdQuote} · frac=${config.rebalanceFraction} · interval=${config.checkIntervalSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.targetallocation;
}

export function stopTargetAllocation(): void {
  const store = getStore();
  stopTimer(store);
  if (store.targetallocation && store.targetallocation.status === "monitoring") {
    store.targetallocation.status = "idle";
    store.events.push({ t: Date.now(), message: "Target allocation stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.targetallocation = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.targetallocation && store.targetallocation.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { targetallocation: store.targetallocation, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.targetallocation || store.targetallocation.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.targetallocation.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.targetallocation, price, now);
  store.targetallocation = state;
  store.ticks.push({ t: now, price, positions: snapPositions(state) });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
