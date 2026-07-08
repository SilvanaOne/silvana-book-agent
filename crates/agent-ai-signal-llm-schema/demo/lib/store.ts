import { emitPrompt, initState, step, type AiSignalConfig, type AiSignalState } from "./ai-signal-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: AiSignalState | null; events: EventEntry[]; timer: ReturnType<typeof setInterval> | null };
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __aiSignalStore?: StoreState };
function getStore(): StoreState { if (!g.__aiSignalStore) g.__aiSignalStore = { agent: null, events: [], timer: null }; return g.__aiSignalStore; }

export function startAgent(config: AiSignalConfig): AiSignalState {
  const s = getStore();
  stopTimer(s);
  const now = Date.now();
  s.agent = initState(config, now);
  s.events = [{ t: now, message: `ai-signal started — model=${config.model} min_conf=${config.minConfidence}` }];
  startTimer(s);
  return s.agent;
}
export function stopAgent(): void { const s = getStore(); stopTimer(s); if (s.agent && s.agent.status === "running") { s.agent.status = "idle"; s.events.push({ t: Date.now(), message: "ai-signal stopped" }); } }
export function resetStore(): void { const s = getStore(); stopTimer(s); s.agent = null; s.events = []; }
export function manualEmit(prompt: string): AiSignalState | null {
  const s = getStore();
  if (!s.agent) return null;
  const { state, log } = emitPrompt(s.agent, prompt, Date.now());
  s.agent = state;
  const now = Date.now();
  for (const l of log) s.events.push({ t: now, message: l });
  while (s.events.length > MAX_EVENTS) s.events.shift();
  return s.agent;
}
export function snapshot() { const s = getStore(); return { agent: s.agent, events: s.events }; }

function startTimer(s: StoreState) { s.timer = setInterval(() => tick(s), 1000); }
function stopTimer(s: StoreState) { if (s.timer) { clearInterval(s.timer); s.timer = null; } }
function tick(s: StoreState) {
  if (!s.agent || s.agent.status !== "running") { stopTimer(s); return; }
  const now = Date.now();
  const { state, log } = step(s.agent, 0, now);
  s.agent = state;
  for (const e of log) s.events.push({ t: now, message: e });
  while (s.events.length > MAX_EVENTS) s.events.shift();
}
