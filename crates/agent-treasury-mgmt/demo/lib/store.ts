import { initState, step, type TreasuryConfig, type TreasuryState } from "./treasury-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  agent: TreasuryState | null;
  driverPrice: number;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __treasuryDemoStore?: StoreState };
function getStore(): StoreState { if (!g.__treasuryDemoStore) g.__treasuryDemoStore = { agent: null, driverPrice: 1, ticks: [], events: [], timer: null, walk: { driftPerTick: 0, volPerTick: 0.005 }, priceOverride: null }; return g.__treasuryDemoStore; }

export function startAgent(config: TreasuryConfig): TreasuryState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.agent = initState(config, now);
  const firstMarket = Object.keys(config.startingPrices)[0];
  store.driverPrice = firstMarket ? config.startingPrices[firstMarket] : 1;
  store.ticks = [{ t: now, price: store.driverPrice }];
  store.events = [{ t: now, message: `treasury-mgmt started — ${config.targets.length} targets, approval > ${config.approvalThresholdQuote}` }];
  store.priceOverride = null;
  startTimer(store);
  return store.agent;
}
export function stopAgent(): void { const s = getStore(); stopTimer(s); if (s.agent && s.agent.status === "running") { s.agent.status = "idle"; s.events.push({ t: Date.now(), message: "treasury-mgmt stopped" }); } }
export function resetStore(): void { const s = getStore(); stopTimer(s); s.agent = null; s.ticks = []; s.events = []; s.driverPrice = 1; s.priceOverride = null; }
export function jumpPrice(to: number): void { const s = getStore(); if (s.agent && s.agent.status === "running") s.priceOverride = to; }
export function updateWalk(walk: Partial<WalkParams>): void { const s = getStore(); s.walk = { ...s.walk, ...walk }; }
export function snapshot() { const s = getStore(); return { agent: s.agent, ticks: s.ticks, events: s.events, walk: s.walk }; }

function startTimer(s: StoreState) { s.timer = setInterval(() => tick(s), 1000); }
function stopTimer(s: StoreState) { if (s.timer) { clearInterval(s.timer); s.timer = null; } }
function tick(s: StoreState) {
  if (!s.agent || s.agent.status !== "running") { stopTimer(s); return; }
  const override = s.priceOverride;
  s.priceOverride = null;
  s.driverPrice = nextPrice(s.driverPrice, s.walk, override);
  const now = Date.now();
  const { state, log } = step(s.agent, s.driverPrice, now);
  s.agent = state;
  s.ticks.push({ t: now, price: s.driverPrice });
  if (s.ticks.length > MAX_TICKS) s.ticks.shift();
  for (const e of log) s.events.push({ t: now, message: e });
  while (s.events.length > MAX_EVENTS) s.events.shift();
}
