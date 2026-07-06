import { initState, rotate, tamperSlice, verify, type RetentionConfig, type RetentionState } from "./retention-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: RetentionState | null; events: EventEntry[] };
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __retentionDemoStore?: StoreState };
function getStore(): StoreState { if (!g.__retentionDemoStore) g.__retentionDemoStore = { agent: null, events: [] }; return g.__retentionDemoStore; }

export function startAgent(config: RetentionConfig): RetentionState {
  const s = getStore();
  s.agent = initState(config);
  const { state, log } = rotate(s.agent);
  s.agent = state;
  const now = Date.now();
  s.events = [{ t: now, message: `audit-retention started — weekly=${config.weekly} retention=${config.retentionDays ?? "∞"}d` }];
  for (const l of log) s.events.push({ t: now, message: l });
  return s.agent;
}
export function reRotate(): RetentionState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = rotate(s.agent);
  s.agent = state;
  pushEvents(s, log);
  return s.agent;
}
export function verifyChain(): RetentionState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = verify(s.agent);
  s.agent = state;
  pushEvents(s, log);
  return s.agent;
}
export function tamperAt(num: number): RetentionState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = tamperSlice(s.agent, num);
  s.agent = state;
  pushEvents(s, log);
  return s.agent;
}
export function resetStore(): void { const s = getStore(); s.agent = null; s.events = []; }
export function snapshot() { const s = getStore(); return { agent: s.agent, events: s.events }; }

function pushEvents(s: StoreState, log: string[]) {
  const now = Date.now();
  for (const l of log) s.events.push({ t: now, message: l });
  while (s.events.length > MAX_EVENTS) s.events.shift();
}
