import { initState, step, type InventoryMgmtConfig, type InventoryMgmtState } from "./inventorymgmt-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number; balance: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  inventorymgmt: InventoryMgmtState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __inventorymgmtDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__inventorymgmtDemoStore) {
    g.__inventorymgmtDemoStore = {
      inventorymgmt: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__inventorymgmtDemoStore;
}

export function startInventoryMgmt(config: InventoryMgmtConfig): InventoryMgmtState {
  const store = getStore();
  stopTimer(store);
  store.inventorymgmt = initState(config);
  store.ticks = [{ t: Date.now(), price: config.startingPrice, balance: config.startingBalance }];
  store.events = [
    {
      t: Date.now(),
      message: `Inventory-mgmt started: ${config.market} ${config.instrument}, target ${config.target} ±${config.tolerance}, chunk ${config.chunkSize}, check every ${config.checkIntervalSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.inventorymgmt;
}

export function stopInventoryMgmt(): void {
  const store = getStore();
  stopTimer(store);
  if (store.inventorymgmt && store.inventorymgmt.status === "monitoring") {
    store.inventorymgmt.status = "idle";
    store.events.push({ t: Date.now(), message: "Inventory-mgmt stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.inventorymgmt = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.inventorymgmt && store.inventorymgmt.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return { inventorymgmt: store.inventorymgmt, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.inventorymgmt || store.inventorymgmt.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.inventorymgmt.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.inventorymgmt, price, now);
  store.inventorymgmt = state;
  store.ticks.push({ t: now, price, balance: state.currentBalance });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
