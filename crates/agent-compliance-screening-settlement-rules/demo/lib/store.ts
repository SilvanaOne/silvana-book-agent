import { initState, step, type ComplianceConfig, type ComplianceState } from "./compliance-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = { agent: ComplianceState | null; events: EventEntry[]; timer: ReturnType<typeof setInterval> | null };

const MAX_EVENTS = 200;
const g = globalThis as unknown as { __compScreenStore?: StoreState };

function getStore(): StoreState {
  if (!g.__compScreenStore) g.__compScreenStore = { agent: null, events: [], timer: null };
  return g.__compScreenStore;
}

export function startAgent(config: ComplianceConfig): ComplianceState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.agent = initState(config, now);
  store.events = [{ t: now, message: `compliance-screening started — rate=${config.eventRatePerSec}/s emit_accepts=${config.emitAccepts}` }];
  startTimer(store);
  return store.agent;
}

export function stopAgent(): void {
  const store = getStore();
  stopTimer(store);
  if (store.agent && store.agent.status === "running") {
    store.agent.status = "idle";
    store.events.push({ t: Date.now(), message: "compliance-screening stopped" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.agent = null;
  store.events = [];
}

export function snapshot() {
  const store = getStore();
  return { agent: store.agent, events: store.events };
}

function startTimer(store: StoreState) { store.timer = setInterval(() => tick(store), 500); }
function stopTimer(store: StoreState) { if (store.timer) { clearInterval(store.timer); store.timer = null; } }
function tick(store: StoreState) {
  if (!store.agent || store.agent.status !== "running") { stopTimer(store); return; }
  const now = Date.now();
  const { state, log } = step(store.agent, 0, now);
  store.agent = state;
  for (const e of log) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
}
