import { ingestHeadline, initState, step, type NewsConfig, type NewsState } from "./news-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: NewsState | null; events: EventEntry[]; timer: ReturnType<typeof setInterval> | null };
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __newsStore?: StoreState };
function getStore(): StoreState { if (!g.__newsStore) g.__newsStore = { agent: null, events: [], timer: null }; return g.__newsStore; }

export function startAgent(config: NewsConfig): NewsState {
  const s = getStore();
  stopTimer(s);
  const now = Date.now();
  s.agent = initState(config, now);
  s.events = [{ t: now, message: `news-sentiment started — threshold=${config.threshold} pool=${config.headlinePool.length}` }];
  startTimer(s);
  return s.agent;
}
export function stopAgent(): void { const s = getStore(); stopTimer(s); if (s.agent && s.agent.status === "running") { s.agent.status = "idle"; s.events.push({ t: Date.now(), message: "news-sentiment stopped" }); } }
export function resetStore(): void { const s = getStore(); stopTimer(s); s.agent = null; s.events = []; }
export function manualIngest(text: string): NewsState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = ingestHeadline(s.agent, text, Date.now());
  s.agent = state;
  const now = Date.now();
  for (const l of log) s.events.push({ t: now, message: l });
  while (s.events.length > MAX_EVENTS) s.events.shift();
  return s.agent;
}
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
