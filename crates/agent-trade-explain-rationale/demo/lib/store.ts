import { initState, runExplain, type ExplainConfig, type ExplainState } from "./explain-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: ExplainState | null; events: EventEntry[] };
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __explainStore?: StoreState };
function getStore(): StoreState { if (!g.__explainStore) g.__explainStore = { agent: null, events: [] }; return g.__explainStore; }

export function startAgent(config: ExplainConfig): ExplainState {
  const s = getStore();
  s.agent = initState(config);
  const { state, log } = runExplain(s.agent);
  s.agent = state;
  const now = Date.now();
  s.events = [{ t: now, message: `trade-explain loaded — model=${config.model}` }];
  for (const l of log) s.events.push({ t: now, message: l });
  return s.agent;
}
export function rerun(): ExplainState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = runExplain(s.agent);
  s.agent = state;
  const now = Date.now();
  for (const l of log) s.events.push({ t: now, message: l });
  while (s.events.length > MAX_EVENTS) s.events.shift();
  return s.agent;
}
export function resetStore(): void { const s = getStore(); s.agent = null; s.events = []; }
export function snapshot() { const s = getStore(); return { agent: s.agent, events: s.events }; }
