import { initState, step, type LegalConfig, type LegalState } from "./legal-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: LegalState | null; events: EventEntry[]; timer: ReturnType<typeof setInterval> | null };

const MAX_EVENTS = 200;
const g = globalThis as unknown as { __legalDemoStore?: StoreState };
function getStore(): StoreState { if (!g.__legalDemoStore) g.__legalDemoStore = { agent: null, events: [], timer: null }; return g.__legalDemoStore; }

export function startAgent(config: LegalConfig): LegalState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.agent = initState(config, now);
  store.events = [{ t: now, message: `legal-compliance started — ${Object.keys(config.policy.jurisdictions).length} jurisdictions` }];
  startTimer(store);
  return store.agent;
}
export function stopAgent(): void { const s = getStore(); stopTimer(s); if (s.agent && s.agent.status === "running") { s.agent.status = "idle"; s.events.push({ t: Date.now(), message: "legal-compliance stopped" }); } }
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
