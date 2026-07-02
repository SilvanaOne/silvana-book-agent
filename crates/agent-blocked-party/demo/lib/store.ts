import { initState, setBlocklist, step, type BlockedPartyConfig, type BlockedPartyState } from "./blockedparty-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  blockedparty: BlockedPartyState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;

const g = globalThis as unknown as { __blockedpartyDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__blockedpartyDemoStore) {
    g.__blockedpartyDemoStore = {
      blockedparty: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__blockedpartyDemoStore;
}

export function startBlockedParty(config: BlockedPartyConfig): BlockedPartyState {
  const store = getStore();
  stopTimer(store);
  store.blockedparty = initState(config);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  store.events = [
    {
      t: Date.now(),
      message: `Blocked-party started: blocklist=${config.blocklist.length} entries, reload=${config.reloadSecs}s, arrival λ=${config.settlementArrivalPerTick}/tick, hit-rate=${(config.blockedPartyProbability * 100).toFixed(1)}%`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.blockedparty;
}

export function stopBlockedParty(): void {
  const store = getStore();
  stopTimer(store);
  if (store.blockedparty && store.blockedparty.status === "monitoring") {
    store.blockedparty.status = "idle";
    store.events.push({ t: Date.now(), message: "Blocked-party stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.blockedparty = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.blockedparty && store.blockedparty.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function reloadBlocklist(next: readonly string[]): BlockedPartyState | null {
  const store = getStore();
  if (!store.blockedparty) return null;
  const now = Date.now();
  const { state, events } = setBlocklist(store.blockedparty, next, now);
  store.blockedparty = state;
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  return store.blockedparty;
}

export function snapshot() {
  const store = getStore();
  return { blockedparty: store.blockedparty, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.blockedparty || store.blockedparty.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.blockedparty.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.blockedparty, price, now);
  store.blockedparty = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
