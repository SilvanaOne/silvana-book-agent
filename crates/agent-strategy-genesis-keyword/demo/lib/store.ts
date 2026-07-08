import { compile, initState, type GenesisState, type Step } from "./genesis-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: GenesisState | null; events: EventEntry[] };
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __genesisStore?: StoreState };
function getStore(): StoreState { if (!g.__genesisStore) g.__genesisStore = { agent: null, events: [] }; return g.__genesisStore; }

export function ensureLoaded(): GenesisState {
  const s = getStore();
  if (!s.agent) {
    s.agent = initState();
    s.events = [{ t: Date.now(), message: "strategy-genesis ready — send a spec to compile" }];
  }
  return s.agent;
}

export function compileSpec(spec: string): { state: GenesisState; step: Step } {
  const s = getStore();
  const state = ensureLoaded();
  const step = compile(state, spec, Date.now());
  const now = Date.now();
  const msg = step.error
    ? `ERR   #${step.seq} — ${step.error} (spec: "${step.spec.slice(0, 40)}")`
    : `OK    #${step.seq} → ${step.algorithm}/${step.side}/${step.market}/${step.total}`;
  s.events.push({ t: now, message: msg });
  while (s.events.length > MAX_EVENTS) s.events.shift();
  return { state, step };
}

export function resetStore(): void { const s = getStore(); s.agent = null; s.events = []; }
export function snapshot() { const s = getStore(); return { agent: s.agent, events: s.events }; }
