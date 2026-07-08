import {
  initState,
  step,
  decide,
  purge as purgeQueue,
  type HumanApprovalConfig,
  type HumanApprovalState,
} from "./humanapproval-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  humanapproval: HumanApprovalState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;

const g = globalThis as unknown as { __humanapprovalDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__humanapprovalDemoStore) {
    g.__humanapprovalDemoStore = {
      humanapproval: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__humanapprovalDemoStore;
}

export function startHumanApproval(config: HumanApprovalConfig): HumanApprovalState {
  const store = getStore();
  stopTimer(store);
  store.humanapproval = initState(config, config.startingPrice);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  const auto = config.autoApprovalEnabled ? `auto<${config.autoApprovalThreshold}` : "auto off";
  store.events = [
    {
      t: Date.now(),
      message: `HA started: ${config.market}, arrival ${config.orderArrivalPerTick}/tick, ${auto}, reviewer=${config.reviewerName}`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.humanapproval;
}

export function stopHumanApproval(): void {
  const store = getStore();
  stopTimer(store);
  if (store.humanapproval && store.humanapproval.status === "monitoring") {
    store.humanapproval.status = "idle";
    store.events.push({ t: Date.now(), message: "HA stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.humanapproval = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.humanapproval && store.humanapproval.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function approveOrder(id: string, by?: string, reason?: string): { ok: boolean; error?: string } {
  const store = getStore();
  if (!store.humanapproval) return { ok: false, error: "HA not started" };
  const now = Date.now();
  const res = decide(store.humanapproval, id, "approve", by, reason, now);
  if (!res.ok) return { ok: false, error: res.error };
  if (res.event) pushEvent(store, res.event, now);
  return { ok: true };
}

export function rejectOrder(id: string, by?: string, reason?: string): { ok: boolean; error?: string } {
  const store = getStore();
  if (!store.humanapproval) return { ok: false, error: "HA not started" };
  const now = Date.now();
  const res = decide(store.humanapproval, id, "reject", by, reason, now);
  if (!res.ok) return { ok: false, error: res.error };
  if (res.event) pushEvent(store, res.event, now);
  return { ok: true };
}

export function purgeOrders(): { removed: number } {
  const store = getStore();
  if (!store.humanapproval) return { removed: 0 };
  const now = Date.now();
  const res = purgeQueue(store.humanapproval, now);
  pushEvent(store, res.event, now);
  return { removed: res.removed };
}

export function snapshot() {
  const store = getStore();
  return { humanapproval: store.humanapproval, ticks: store.ticks, events: store.events, walk: store.walk };
}

function pushEvent(store: StoreState, msg: string, t: number) {
  store.events.push({ t, message: msg });
  while (store.events.length > MAX_EVENTS) store.events.shift();
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
  if (!store.humanapproval || store.humanapproval.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.humanapproval.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.humanapproval, price, now);
  store.humanapproval = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) pushEvent(store, e, now);
  if (state.status !== "monitoring") stopTimer(store);
}
