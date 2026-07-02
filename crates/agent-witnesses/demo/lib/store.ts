import { initState, step, type WitnessesConfig, type WitnessesState } from "./witnesses-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  witnesses: WitnessesState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __witnessesDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__witnessesDemoStore) {
    g.__witnessesDemoStore = {
      witnesses: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__witnessesDemoStore;
}

export function startWitnesses(config: WitnessesConfig): WitnessesState {
  const store = getStore();
  stopTimer(store);
  store.witnesses = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  const handlerList = config.handlers.map((h) => `${h.eventKind}→${h.command}`).join(", ");
  store.events = [
    {
      t: Date.now(),
      message: `Witnesses started: ${config.handlers.length} handlers${config.market ? ` (market=${config.market})` : ""}: ${handlerList}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.witnesses;
}

export function stopWitnesses(): void {
  const store = getStore();
  stopTimer(store);
  if (store.witnesses && store.witnesses.status === "monitoring") {
    store.witnesses.status = "idle";
    store.events.push({ t: Date.now(), message: "Witnesses stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.witnesses = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.witnesses && store.witnesses.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { witnesses: store.witnesses, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.witnesses || store.witnesses.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.witnesses.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.witnesses, price, now);
  store.witnesses = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
