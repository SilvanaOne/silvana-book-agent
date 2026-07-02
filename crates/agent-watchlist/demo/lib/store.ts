import { initState, step, type WatchlistConfig, type WatchlistState } from "./watchlist-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

// A tick is a single (t, price) sample of the FIRST market's mid — used only
// to keep the DemoTools price-nudger + a placeholder line for compatibility
// with the scaffolded infra. The real per-market series lives in
// state.snapshots and is rendered by WatchlistChart.
export type Tick = Readonly<{ t: number; price: number; ema: number | null }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  watchlist: WatchlistState | null;
  ticks: Tick[];                       // ticks of primary market
  perMarketTicks: Record<string, Tick[]>; // rolling series per market
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __watchlistDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__watchlistDemoStore) {
    g.__watchlistDemoStore = {
      watchlist: null,
      ticks: [],
      perMarketTicks: {},
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__watchlistDemoStore;
}

export function startWatchlist(config: WatchlistConfig): WatchlistState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.watchlist = initState(config, now);
  store.ticks = [];
  store.perMarketTicks = {};
  for (const s of store.watchlist.snapshots) {
    store.perMarketTicks[s.market] = [{ t: now, price: s.price, ema: null }];
  }
  const primary = store.watchlist.snapshots[0];
  if (primary) store.ticks = [{ t: now, price: primary.price, ema: null }];
  store.events = [
    {
      t: now,
      message: `Watchlist started: ${config.markets.join(", ")} (depth=${config.depthLevels}, poll=${config.pollSecs}s, prices=${config.includePrices}, orderbook=${config.includeOrderbook})`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return store.watchlist;
}

export function stopWatchlist(): void {
  const store = getStore();
  stopTimer(store);
  if (store.watchlist && store.watchlist.status === "monitoring") {
    store.watchlist.status = "idle";
    store.events.push({ t: Date.now(), message: "Watchlist stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.watchlist = null;
  store.ticks = [];
  store.perMarketTicks = {};
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.watchlist && store.watchlist.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function snapshot() {
  const store = getStore();
  return {
    watchlist: store.watchlist,
    ticks: store.ticks,
    perMarketTicks: store.perMarketTicks,
    events: store.events,
    walk: store.walk,
  };
}

function startTimer(store: StoreState) {
  const secs = store.watchlist?.config.pollSecs ?? 2;
  const ms = Math.max(250, Math.round(secs * 1000));
  store.timer = setInterval(() => tick(store), ms);
}
function stopTimer(store: StoreState) {
  if (store.timer) {
    clearInterval(store.timer);
    store.timer = null;
  }
}
function tick(store: StoreState) {
  if (!store.watchlist || store.watchlist.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const now = Date.now();

  // Apply optional primary-market jump before stepping.
  const primary = store.watchlist.snapshots[0];
  const override = store.priceOverride;
  store.priceOverride = null;
  if (primary && typeof override === "number" && override > 0) {
    primary.price = override;
  } else if (primary) {
    // Fold the operator walk knobs into the primary market so the
    // DemoTools UI still meaningfully affects the tape.
    primary.price = nextPrice(primary.price, store.walk, null);
  }

  const { state, events } = step(store.watchlist, 0, now);
  store.watchlist = state;

  // Append a tick per market.
  for (const s of state.snapshots) {
    const list = store.perMarketTicks[s.market] ?? [];
    list.push({ t: now, price: s.price, ema: null });
    if (list.length > MAX_TICKS) list.shift();
    store.perMarketTicks[s.market] = list;
  }
  const p = state.snapshots[0];
  if (p) {
    store.ticks.push({ t: now, price: p.price, ema: null });
    if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  }

  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
