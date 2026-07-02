import { initState, step, type LiquiditySeekingConfig, type LiquiditySeekingState } from "./liquidityseeking-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  liquidityseeking: LiquiditySeekingState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 160;

const g = globalThis as unknown as { __liquidityseekingDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__liquidityseekingDemoStore) {
    g.__liquidityseekingDemoStore = {
      liquidityseeking: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.005 },
      priceOverride: null,
    };
  }
  return g.__liquidityseekingDemoStore;
}

export function startLiquiditySeeking(config: LiquiditySeekingConfig): LiquiditySeekingState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.liquidityseeking = initState(config, config.startingPrice, now);
  store.ticks = [{ t: now, price: config.startingPrice }];
  store.events = [
    {
      t: now,
      message: `LS started: ${config.market} ${config.side} total=${config.total} maxSlip=${config.maxSlippageBps}bps maxChunk=${config.maxChunk} depth=${config.depth}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.liquidityseeking;
}

export function stopLiquiditySeeking(): void {
  const store = getStore();
  stopTimer(store);
  if (store.liquidityseeking && store.liquidityseeking.status === "monitoring") {
    store.liquidityseeking.status = "idle";
    store.events.push({ t: Date.now(), message: "LS stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.liquidityseeking = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.liquidityseeking && store.liquidityseeking.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { liquidityseeking: store.liquidityseeking, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.liquidityseeking || store.liquidityseeking.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.liquidityseeking.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.liquidityseeking, price, now);
  store.liquidityseeking = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
