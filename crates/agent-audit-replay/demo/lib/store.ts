import { initState, reloadHistory, runReplay, type ReplayConfig, type ReplayState } from "./replay-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: ReplayState | null; events: EventEntry[] };
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __replayDemoStore?: StoreState };
function getStore(): StoreState { if (!g.__replayDemoStore) g.__replayDemoStore = { agent: null, events: [] }; return g.__replayDemoStore; }

export function startAgent(config: ReplayConfig): ReplayState {
  const s = getStore();
  s.agent = initState(config);
  const { state, log } = runReplay(s.agent);
  s.agent = state;
  const now = Date.now();
  s.events = [{ t: now, message: `audit-replay loaded — ${state.history.length} records` }];
  for (const l of log) s.events.push({ t: now, message: l });
  return s.agent;
}
export function reload(): ReplayState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = reloadHistory(s.agent);
  s.agent = state;
  const now = Date.now();
  for (const l of log) s.events.push({ t: now, message: l });
  while (s.events.length > MAX_EVENTS) s.events.shift();
  return s.agent;
}
export function rerun(): ReplayState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = runReplay(s.agent);
  s.agent = state;
  const now = Date.now();
  for (const l of log) s.events.push({ t: now, message: l });
  while (s.events.length > MAX_EVENTS) s.events.shift();
  return s.agent;
}
export function resetStore(): void { const s = getStore(); s.agent = null; s.events = []; }
export function snapshot() { const s = getStore(); return { agent: s.agent, events: s.events }; }
