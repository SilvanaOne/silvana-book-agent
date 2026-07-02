import {
  initState,
  step,
  manualSign,
  manualVerify,
  rotateKey,
  type SignatureConfig,
  type SignatureState,
  type SignOperation,
} from "./signature-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type Tick = Readonly<{ t: number; price: number }>;
export type EventEntry = Readonly<{ t: number; message: string }>;

type StoreState = {
  signature: SignatureState | null;
  ticks: Tick[];
  events: EventEntry[];
  timer: ReturnType<typeof setInterval> | null;
  walk: WalkParams;
  priceOverride: number | null;
};

const MAX_TICKS = 400;
const MAX_EVENTS = 120;

const g = globalThis as unknown as { __signatureDemoStore?: StoreState };

function getStore(): StoreState {
  if (!g.__signatureDemoStore) {
    g.__signatureDemoStore = {
      signature: null,
      ticks: [],
      events: [],
      timer: null,
      walk: { driftPerTick: 0, volPerTick: 0.008 },
      priceOverride: null,
    };
  }
  return g.__signatureDemoStore;
}

export type PublicSignatureState = Omit<SignatureState, "privateKey">;

export function startSignature(config: SignatureConfig): PublicSignatureState {
  const store = getStore();
  stopTimer(store);
  store.signature = initState(config);
  store.ticks = [{ t: Date.now(), price: config.startingPrice }];
  store.events = [
    {
      t: Date.now(),
      message: `Signature agent started: auto-demo ${config.autoDemoIntervalSecs}s, tamper ${(config.tamperRate * 100).toFixed(1)}%, pub=${store.signature.publicKey.slice(0, 16)}…`,
    },
  ];
  store.priceOverride = null;
  startTimer(store);
  return sanitize(store.signature);
}

export function stopSignature(): void {
  const store = getStore();
  stopTimer(store);
  if (store.signature && store.signature.status === "monitoring") {
    store.signature.status = "idle";
    store.events.push({ t: Date.now(), message: "Signature agent stopped by operator" });
  }
}

export function resetStore(): void {
  const store = getStore();
  stopTimer(store);
  store.signature = null;
  store.ticks = [];
  store.events = [];
  store.priceOverride = null;
}

export function jumpPrice(to: number): void {
  const store = getStore();
  if (store.signature && store.signature.status === "monitoring") store.priceOverride = to;
}

export function updateWalk(walk: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...walk };
}

/** Manual sign — used by /api/signature/sign. Returns {publicKey, signature, seq, message}. */
export function signMessage(message: string, canonical = false): {
  ok: true;
  op: SignOperation;
  publicKey: string;
} | { ok: false; error: string } {
  const store = getStore();
  if (!store.signature) return { ok: false, error: "signature agent not started" };
  const op = manualSign(store.signature, message, Date.now(), canonical);
  store.events.push({ t: op.t, message: `SIGN (manual) msg="${truncate(message, 40)}" sig=${op.output.slice(0, 12)}…` });
  trimEvents(store);
  return { ok: true, op, publicKey: store.signature.publicKey };
}

/** Manual verify — used by /api/signature/verify. */
export function verifyMessage(message: string, signature: string): {
  ok: true;
  op: SignOperation;
} | { ok: false; error: string } {
  const store = getStore();
  if (!store.signature) return { ok: false, error: "signature agent not started" };
  const op = manualVerify(store.signature, message, signature, Date.now());
  store.events.push({
    t: op.t,
    message: `VERIFY (manual) msg="${truncate(message, 40)}" → ${op.verified ? "OK" : "FAIL"}`,
  });
  trimEvents(store);
  return { ok: true, op };
}

/** Rotate keypair — invalidates all prior signatures. */
export function rotateSignatureKey(): { ok: true; op: SignOperation; publicKey: string } | { ok: false; error: string } {
  const store = getStore();
  if (!store.signature) return { ok: false, error: "signature agent not started" };
  const op = rotateKey(store.signature, Date.now());
  store.events.push({ t: op.t, message: `KEY ROTATED pub=${store.signature.publicKey.slice(0, 16)}…` });
  trimEvents(store);
  return { ok: true, op, publicKey: store.signature.publicKey };
}

export function snapshot() {
  const store = getStore();
  return {
    signature: store.signature ? sanitize(store.signature) : null,
    ticks: store.ticks,
    events: store.events,
    walk: store.walk,
  };
}

/** Strip the raw private key before returning state to the client. */
function sanitize(s: SignatureState): PublicSignatureState {
  const { privateKey: _pk, ...rest } = s;
  void _pk;
  return rest;
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
  if (!store.signature || store.signature.status !== "monitoring") {
    stopTimer(store);
    return;
  }
  const override = store.priceOverride;
  store.priceOverride = null;
  const price = nextPrice(store.signature.currentPrice, store.walk, override);
  const now = Date.now();
  const { state, events } = step(store.signature, price, now);
  store.signature = state;
  store.ticks.push({ t: now, price });
  if (store.ticks.length > MAX_TICKS) store.ticks.shift();
  for (const e of events) store.events.push({ t: now, message: e });
  trimEvents(store);
  if (state.status !== "monitoring") stopTimer(store);
}

function trimEvents(store: StoreState) {
  while (store.events.length > MAX_EVENTS) store.events.shift();
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n) + "…" : s;
}
