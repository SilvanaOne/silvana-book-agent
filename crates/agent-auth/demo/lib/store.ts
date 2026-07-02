import {
  initState,
  step,
  generateToken,
  decodeToken,
  verifyToken,
  type AuthConfig,
  type AuthState,
  type JwtToken,
} from "./auth-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  auth: AuthState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __authDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__authDemoStore) {
    g.__authDemoStore = {
      auth: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__authDemoStore;
}

export function startAuth(config: AuthConfig): AuthState {
  const store = getStore();
  stopTimer(store);
  const now = Date.now();
  store.auth = initState(config, config.startingPrice, now);
  store.ticks = [{ t: now, price: config.startingPrice }];
  store.events = [
    {
      t: now,
      message: `Auth started: role=${config.role} ttl=${config.ttlSecs}s auto-refresh=${
        config.autoRefreshEnabled ? `every ${config.autoRefreshIntervalSecs}s` : "off"
      }`,
    },
  ];
  if (store.auth.currentToken) {
    store.events.push({
      t: now,
      message: `TOKEN MINTED: jti=${store.auth.currentToken.payload.jti} role=${store.auth.currentToken.payload.role}`,
    });
  }
  store.priceOverride = null;
  startTimer(store);
  return store.auth;
}

export function stopAuth(): void {
  const store = getStore();
  stopTimer(store);
  if (store.auth && store.auth.status === "monitoring") {
    store.auth.status = "idle";
    store.events.push({ t: Date.now(), message: "Auth stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.auth = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.auth && store.auth.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

export function generateNow(override: { role?: string; ttlSecs?: number }): JwtToken | null {
  const store = getStore();
  if (!store.auth) return null;
  const now = Date.now();
  const { token, events } = generateToken(store.auth, override, now);
  for (const e of events) pushEvent(store, now, e);
  return token;
}

export function decodeCurrent(): { header: unknown; payload: unknown; signature: string } | null {
  const store = getStore();
  if (!store.auth?.currentToken) return null;
  const parsed = decodeToken(store.auth.currentToken.raw);
  store.auth.stats.decoded += 1;
  pushEvent(store, Date.now(), `TOKEN DECODED: jti=${store.auth.currentToken.payload.jti}`);
  return parsed;
}

export function decodeAny(raw: string): { header: unknown; payload: unknown; signature: string } {
  const store = getStore();
  const parsed = decodeToken(raw);
  if (store.auth) store.auth.stats.decoded += 1;
  pushEvent(store, Date.now(), `EXT TOKEN DECODED (${raw.length} bytes)`);
  return parsed;
}

export function verifyCurrent(): { ok: boolean; reason: string } | null {
  const store = getStore();
  if (!store.auth?.currentToken) return null;
  const now = Date.now();
  const res = verifyToken(store.auth.currentToken.raw, now);
  store.auth.stats.verified += 1;
  if (res.ok) store.auth.stats.verifiedOk += 1;
  pushEvent(store, now, `TOKEN VERIFY: ${res.ok ? "OK" : "FAIL"} — ${res.reason}`);
  return { ok: res.ok, reason: res.reason };
}

export function verifyAny(raw: string): { ok: boolean; reason: string } {
  const store = getStore();
  const now = Date.now();
  const res = verifyToken(raw, now);
  if (store.auth) {
    store.auth.stats.verified += 1;
    if (res.ok) store.auth.stats.verifiedOk += 1;
  }
  pushEvent(store, now, `EXT TOKEN VERIFY: ${res.ok ? "OK" : "FAIL"} — ${res.reason}`);
  return { ok: res.ok, reason: res.reason };
}

export function snapshot() {
  const store = getStore();
  return { auth: store.auth, ticks: store.ticks, events: store.events, walk: store.walk };
}

function startTimer(store: StoreState) {
  store.timer = setInterval(() => tick(store), 1000);
}
function stopTimer(store: StoreState) {
  if (store.timer) {
    clearInterval(store.timer);
    store.timer = null;
  }
}
function tick(store: StoreState) {
  if (!store.auth || store.auth.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.auth.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.auth, price, now);
  store.auth = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) pushEvent(store, now, e);
  if (state.status !== "monitoring") stopTimer(store);
}

function pushEvent(store: StoreState, t: number, message: string): void {
  store.events.push({ t, message });
  while (store.events.length > MAX_EVENTS) store.events.shift();
}
