import { initState, step, type ScamConfig, type ScamState } from "./scam-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: ScamState | null; events: EventEntry[]; timer: ReturnType<typeof setInterval> | null };

const MAX_EVENTS = 200;
const g = globalThis as unknown as { __scamDemoStore?: StoreState };
function getStore(): StoreState { if (!g.__scamDemoStore) g.__scamDemoStore = { agent: null, events: [], timer: null }; return g.__scamDemoStore; }

export function startAgent(config: ScamConfig): ScamState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.agent = initState(config, now);
  store.events = [{ t: now, message: `scam-screening started — ${config.categories.length} categories` }];
  startTimer(store);
  return store.agent;
}
export function stopAgent(): void { const s = getStore(); stopTimer(s); if (s.agent && s.agent.status === "running") { s.agent.status = "idle"; s.events.push({ t: Date.now(), message: "scam-screening stopped" }); } }
export function resetStore(): void { const s = getStore(); stopTimer(s); s.agent = null; s.events = []; }
export function snapshot() { const s = getStore(); return { agent: s.agent, events: s.events }; }
function startTimer(s: StoreState) { s.timer = setInterval(() => tick(s), 500); }
function stopTimer(s: StoreState) { if (s.timer) { clearInterval(s.timer); s.timer = null; } }
function tick(s: StoreState) {
  if (!s.agent || s.agent.status !== "running") { stopTimer(s); return; }
  const now = Date.now();
  const { state, log } = step(s.agent, 0, now);
  s.agent = state;
  for (const e of log) s.events.push({ t: now, message: e });
  while (s.events.length > MAX_EVENTS) s.events.shift();
}
