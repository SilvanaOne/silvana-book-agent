import { initState, step, type TestRunConfig, type TestRunState } from "./testrun-engine";

export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  agent: TestRunState | null;
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
};

const MAX_EVENTS = 200;

const g = globalThis as unknown as { __testrunDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__testrunDemoStore) {
    g.__testrunDemoStore = { agent: null, events: [], timer: null };
  }
  return g.__testrunDemoStore;
}

export function startAgent(config: TestRunConfig): TestRunState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.agent = initState(config, now);
  store.events = [{ t: now, message: `Starting test-run — endpoint=${config.endpoint} party=${config.partyId} market=${config.market || "(none)"} interval=${config.runIntervalSecs}s` }];
  startTimer(store);
  return store.agent;
}

export function stopAgent(): void {
  const store = getStore();
  stopTimer(store);
  if (store.agent && store.agent.status === "running") {
    store.agent.status = "idle";
    store.events.push({ t: Date.now(), message: "test-run stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.agent = null;
  store.events = [];
}

export function triggerRun(): void {
  const store = getStore();
  if (!store.agent) return;
  const now = Date.now();
  store.agent.status = "running";
  store.agent.nextRunAt = now;
  store.events.push({ t: now, message: "Manual run triggered" });
  if (!store.timer) startTimer(store);
}

export function snapshot() {
  const store = getStore();
  return { agent: store.agent, events: store.events };
}

function startTimer(store: StoreState) {
  store.timer = setInterval(() => tick(store), 500);
}
function stopTimer(store: StoreState) {
  if (store.timer) {
    clearInterval(store.timer);
    store.timer = null;
  }
}
function tick(store: StoreState) {
  if (!store.agent) { stopTimer(store); return; }
  const now = Date.now();
  const { state, events } = step(store.agent, 0, now);
  store.agent = state;
  for (const e of events) store.events.push({ t: now, message: e });
  while (store.events.length > MAX_EVENTS) store.events.shift();
  if (state.status !== "running" && !state.currentRun && state.nextRunAt === null) stopTimer(store);
}
