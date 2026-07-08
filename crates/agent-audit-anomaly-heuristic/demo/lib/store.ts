import { initState, reloadHistory, scan, type AnomalyConfig, type AnomalyState } from "./anomaly-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: AnomalyState | null; events: EventEntry[] };
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __anomalyDemoStore?: StoreState };
function getStore(): StoreState { if (!g.__anomalyDemoStore) g.__anomalyDemoStore = { agent: null, events: [] }; return g.__anomalyDemoStore; }

export function startAgent(config: AnomalyConfig): AnomalyState {
  const s = getStore();
  s.agent = initState(config);
  const { state, log } = scan(s.agent);
  s.agent = state;
  const now = Date.now();
  s.events = [{ t: now, message: `audit-anomaly loaded ${state.history.length} records` }];
  for (const l of log) s.events.push({ t: now, message: l });
  return s.agent;
}
export function reload(): AnomalyState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = reloadHistory(s.agent);
  s.agent = state;
  push(s, log);
  return s.agent;
}
export function reScan(): AnomalyState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = scan(s.agent);
  s.agent = state;
  push(s, log);
  return s.agent;
}
export function resetStore(): void { const s = getStore(); s.agent = null; s.events = []; }
export function snapshot() { const s = getStore(); return { agent: s.agent, events: s.events }; }

function push(s: StoreState, log: string[]) {
  const now = Date.now();
  for (const l of log) s.events.push({ t: now, message: l });
  while (s.events.length > MAX_EVENTS) s.events.shift();
}
