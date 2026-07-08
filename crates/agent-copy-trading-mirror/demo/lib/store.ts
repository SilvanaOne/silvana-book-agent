import { initState, step, type CopyConfig, type CopyState } from "./copy-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;
type StoreState = { agent: CopyState | null; events: EventEntry[]; timer: ReturnType<typeof setInterval> | null };
const MAX_EVENTS = 200;
const g = globalThis as unknown as { __copyDemoStore?: StoreState };
function getStore(): StoreState { if (!g.__copyDemoStore) g.__copyDemoStore = { agent: null, events: [], timer: null }; return g.__copyDemoStore; }

export function startAgent(config: CopyConfig): CopyState {
  const s = getStore();
  stopTimer(s);
  const now = Date.now();
  s.agent = initState(config, now);
  s.events = [{ t: now, message: `copy-trading started — leader=${config.leader} → follower=${config.follower} scale=${config.scale}` }];
  startTimer(s);
  return s.agent;
}
export function stopAgent(): void { const s = getStore(); stopTimer(s); if (s.agent && s.agent.status === "running") { s.agent.status = "idle"; s.events.push({ t: Date.now(), message: "copy-trading stopped" }); } }
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
