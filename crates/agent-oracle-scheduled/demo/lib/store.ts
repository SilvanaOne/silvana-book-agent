import { initState, step, type OracleConfig, type OracleState, type PublishRecord } from "./oracle-engine";

export type MarketTick = Readonly<{ t: number; market: string; price: number; published: boolean }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  oracle: OracleState | null;
  ticks: MarketTick[];               // flat, all markets interleaved (bounded)
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  // Kept for legacy /api/price/walk + /api/price/jump routes; not used by
  // the oracle engine (which walks internally). Left as a no-op sink.
  walk: { driftPerTick: number; volPerTick: number };
  priceOverride: number | null;
};

const MAX_TICKS = 2000;             // ~400 ticks × 5 markets budget
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __oracleDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__oracleDemoStore) {
    g.__oracleDemoStore = {
      oracle: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.005 },
      priceOverride: null,
    };
  }
  return g.__oracleDemoStore;
}

export function startOracle(config: OracleConfig): OracleState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.oracle = initState(config, now);
  store.ticks = store.oracle.currentPrices.map((mp) => ({ t: now, market: mp.market, price: mp.price, published: false }));
  store.events = [
    {
      t: now,
      message: `Oracle started: markets=[${config.markets.join(", ")}], source=${config.source}, poll=${config.pollSecs}s`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.oracle;
}

export function stopOracle(): void {
  const store = getStore();
  stopTimer(store);
  if (store.oracle && store.oracle.status === "publishing") {
    store.oracle.status = "idle";
    store.events.push({ t: Date.now(), message: "Oracle stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.oracle = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(_to: number): void {
  // Not applicable to the oracle demo (multi-market walks are internal).
  // Kept for parity with the shared API surface.
}

export function updateWalk(_walk: Partial<{ driftPerTick: number; volPerTick: number }>): void {
  // No-op — the oracle engine walks each market independently.
}

export function snapshot() {
  const store = getStore();
  return { oracle: store.oracle, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.oracle || store.oracle.status !== "publishing") {
    stopTimer(store);
    return;
  }
  const now = Date.now();
  const priorPublishedCount = store.oracle.publishedCount;
  const { state, events } = step(store.oracle, 0, now);
  store.oracle = state;

  // A publish happened this tick if publishedCount grew.
  const justPublished = state.publishedCount > priorPublishedCount;
  const publishedMarkets = new Set<string>();
  if (justPublished) {
    const newRecords: PublishRecord[] = state.publishedRecords.slice(-state.currentPrices.length);
    for (const r of newRecords) publishedMarkets.add(r.market);
  }
  for (const mp of state.currentPrices) {
    store.ticks.push({ t: now, market: mp.market, price: mp.price, published: publishedMarkets.has(mp.market) });
  }
  while (store.ticks.length > MAX_TICKS) store.ticks.shift();

  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "publishing") stopTimer(store);
}
