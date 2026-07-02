import {
  initState,
  ingestSignal,
  step,
  type Signal,
  type SignalBotConfig,
  type SignalBotState,
  type Side,
} from "./signalbot-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  signalbot: SignalBotState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
  pendingInjects: Array<Omit<Signal, "seq" | "receivedAt" | "cursorBytes">>;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 160;
const CURSOR_LINE_BYTES = 120;

const g = globalThis as unknown as { __signalbotDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__signalbotDemoStore) {
    g.__signalbotDemoStore = {
      signalbot: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
      pendingInjects: [],
    };
  }
  return g.__signalbotDemoStore;
}

export function startSignalBot(config: SignalBotConfig): SignalBotState {
  const store = getStore();
  stopTimer(store);
  store.signalbot = initState(config);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  store.events = [
    {
      t: Date.now(),
      message: `signal-bot started: file=${config.signalsFilePath} from_end=${config.fromEnd} dry_run=${config.dryRun}`,
    },
  ];
  store.priceOverride = null;
  store.pendingInjects = [];
  startTimer(store);
  return store.signalbot;
}

export function stopSignalBot(): void {
  const store = getStore();
  stopTimer(store);
  if (store.signalbot && store.signalbot.status === "monitoring") {
    store.signalbot.status = "idle";
    store.events.push({ t: Date.now(), message: "signal-bot stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.signalbot = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
  store.pendingInjects = [];
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.signalbot && store.signalbot.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function injectSignal(payload: { market: string; side: Side; quantity: number; price: number; ref?: string }): void {
  const store = getStore();
  if (!store.signalbot || store.signalbot.status !== "monitoring") {
    throw new Error("signal-bot is not running");
  }
  store.pendingInjects.push(payload);
}

export function snapshot() {
  const store = getStore();
  return { signalbot: store.signalbot, ticks: store.ticks, events: store.events, walk: store.walk };
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
  if (!store.signalbot || store.signalbot.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.signalbot.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.signalbot, price, now);
  store.signalbot = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });

  // Drain manual injects — they are ingested at the current tick as if
  // just tailed off the JSONL file.
  if (store.pendingInjects.length > 0 && store.signalbot) {
    const drain = store.pendingInjects;
    store.pendingInjects = [];
    for (const p of drain) {
      const seq = store.signalbot.signalsCount + 1;
      const sig: Signal = {
        seq,
        receivedAt: now,
        market: p.market,
        side: p.side,
        quantity: p.quantity,
        price: p.price,
        ref: p.ref,
        cursorBytes: store.signalbot.cursorBytes + CURSOR_LINE_BYTES,
      };
      const r = ingestSignal(store.signalbot, sig, now);
      store.signalbot = r.state;
      for (const e of r.events) store.events.push({ t: now, message: `[inject] ${e}` });
    }
  }

  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "monitoring") stopTimer(store);
}
