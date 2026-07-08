import { initState, runFilter, runVerify, tamperRecord, type DiscloseConfig, type DiscloseState } from "./disclose-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: DiscloseState | null; events: EventEntry[] };

const MAX_EVENTS = 200;
const g = globalThis as unknown as { __discloseDemoStore?: StoreState };
function getStore(): StoreState { if (!g.__discloseDemoStore) g.__discloseDemoStore = { agent: null, events: [] }; return g.__discloseDemoStore; }

export function startAgent(config: DiscloseConfig): DiscloseState {
  const store = getStore();
  store.agent = initState(config);
  const { state, log } = runFilter(store.agent);
  store.agent = state;
  const now = Date.now();
  store.events = [{ t: now, message: `selective-disclosure loaded: history=${state.history.length}` }];
  for (const e of log) store.events.push({ t: now, message: e });
  return store.agent;
}

export function reloadHistory(): DiscloseState | null {
  const store = getStore();
  if (!store.agent) return null;
  const cfg = store.agent.config;
  return startAgent(cfg);
}

export function reFilter(): DiscloseState | null {
  const store = getStore();
  if (!store.agent) return null;
  const { state, log } = runFilter(store.agent);
  store.agent = state;
  const now = Date.now();
  for (const e of log) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  return store.agent;
}

export function verifyChain(): DiscloseState | null {
  const store = getStore();
  if (!store.agent) return null;
  const { state, log } = runVerify(store.agent);
  store.agent = state;
  const now = Date.now();
  for (const e of log) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  return store.agent;
}

export function tamperAt(seq: number): DiscloseState | null {
  const store = getStore();
  if (!store.agent) return null;
  const { state, log } = tamperRecord(store.agent, seq);
  store.agent = state;
  const now = Date.now();
  for (const e of log) store.events.push({ t: now, message: e });
  return store.agent;
}

export function resetStore(): void {
  const s = getStore();
  s.agent = null;
  s.events = [];
}

export function snapshot() {
  const s = getStore();
  return { agent: s.agent, events: s.events };
}
