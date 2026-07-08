// Signal Bot demo engine. Mirrors crates/agent-signal-bot-jsonl/src/main.rs.
//
// The real agent tails a JSONL file. Each line = one signal
// {"market":"CC-USDC","side":"buy","quantity":"0.5","price":"1.02","ref":"..."}
// and submits one limit order per signal. Byte offset is persisted in a
// sibling `<signals>.cursor` file so restarts don't replay history.
//
// The demo simulates that file-tailing loop entirely in-process:
//   * every tick a Poisson-ish coin flip synthesizes a signal
//   * an operator can inject a signal via the "Inject signal" UI
//   * each signal becomes a SignalOrder (submitted / filled / rejected)
//   * cursor bytes advance ~120 per line, like real JSONL lines
//   * fills follow the same rule as the demo mean-reversion engine:
//       BID  filled when mid ≤ order.price
//       OFFER filled when mid ≥ order.price

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";

export type Signal = Readonly<{
  seq: number;
  receivedAt: number;
  market: string;
  side: Side;
  quantity: number;
  price: number;
  ref?: string;
  cursorBytes: number; // running byte offset after ingest
}>;

export type SignalOrderStatus = "submitted" | "filled" | "cancelled" | "rejected";

export type SignalOrder = Readonly<{
  seq: number;
  t: number;              // epoch ms of ingestion / submission
  signal: Signal;
  orderId: string;
  status: SignalOrderStatus;
  filledAt?: number;
  filledPrice?: number;
  rejectReason?: string;
}>;

export type SignalBotConfig = Readonly<{
  signalsFilePath: string;      // mock, cosmetic
  fromEnd: boolean;             // start from end of file (skip history)
  dryRun: boolean;              // simulate submits but never fills
  signalArrivalPerTick: number; // 0..∞ — expected signals per tick
  startingPrice: number;
  rejectionRate: number;        // 0..1 — random rejection
}>;

export type SignalBotStats = { submitted: number; filled: number; cancelled: number; rejected: number };

export type SignalBotState = {
  config: SignalBotConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  cursorBytes: number;
  signalsCount: number;
  ordersCount: number;
  lastSignalRef: string | null;
  lastSignalAt: number | null;
  signalOrders: SignalOrder[]; // bounded ~40
  stats: SignalBotStats;
};

const MAX_ORDERS = 40;
const CURSOR_LINE_BYTES = 120; // ~ length of a typical signal JSONL line
const SANITY_BAND_PCT = 0.15; // if signal price is >15% from mid → reject as insane
const MARKETS = ["CC-USDC", "BTC-USD"] as const;

export function initState(config: SignalBotConfig): SignalBotState {
  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    // If fromEnd, pretend an existing history file already scrolled past.
    cursorBytes: config.fromEnd ? Math.floor(Math.random() * 100_000) + 1_000 : 0,
    signalsCount: 0,
    ordersCount: 0,
    lastSignalRef: null,
    lastSignalAt: null,
    signalOrders: [],
    stats: { submitted: 0, filled: 0, cancelled: 0, rejected: 0 },
  };
}

/** Poisson-ish: 1 - e^(-λ). Bounded [0,1]. */
function arrivalProb(lambda: number): number {
  if (lambda <= 0) return 0;
  return 1 - Math.exp(-lambda);
}

/** Build a synthetic signal around the current mid. */
function synthesizeSignal(state: SignalBotState, now: number): Signal {
  const market = MARKETS[Math.floor(Math.random() * MARKETS.length)];
  const side: Side = Math.random() < 0.5 ? "buy" : "sell";
  const quantity = round(1 + Math.random() * 9, 3); // 1..10
  const jitter = 1 + (Math.random() * 0.04 - 0.02); // ±2%
  const price = round8(state.currentPrice * jitter);
  const seq = state.signalsCount + 1;
  const ref = `sig-${seq}`;
  const cursorBytes = state.cursorBytes + CURSOR_LINE_BYTES + Math.floor(Math.random() * 20 - 10);
  return { seq, receivedAt: now, market, side, quantity, price, ref, cursorBytes };
}

/** Apply a signal → produce a SignalOrder (submitted or rejected). Pure. */
export function ingestSignal(
  state: SignalBotState,
  signal: Signal,
  now: number,
): { state: SignalBotState; events: string[] } {
  const events: string[] = [];
  state.signalsCount = Math.max(state.signalsCount, signal.seq);
  state.cursorBytes = Math.max(state.cursorBytes, signal.cursorBytes);
  state.lastSignalRef = signal.ref ?? null;
  state.lastSignalAt = signal.receivedAt;

  const label: OrderType = signal.side === "buy" ? "BID" : "OFFER";
  events.push(
    `SIGNAL #${signal.seq} ${signal.market} ${label} ${signal.quantity} @ ${fmt(signal.price)} ref=${signal.ref ?? "-"}`,
  );

  // Sanity band → reject as if the on-chain client refused.
  const diff = Math.abs(signal.price - state.currentPrice) / state.currentPrice;
  const rand = Math.random();
  let reason: string | null = null;
  if (diff > SANITY_BAND_PCT) reason = "price outside sanity band";
  else if (rand < state.config.rejectionRate) reason = "rejected by gateway";

  const seq = state.ordersCount + 1;
  state.ordersCount = seq;

  if (reason) {
    const order: SignalOrder = {
      seq,
      t: now,
      signal,
      orderId: `rej-${seq}`,
      status: "rejected",
      rejectReason: reason,
    };
    pushOrder(state, order);
    state.stats.rejected += 1;
    events.push(`  → REJECTED #${seq}: ${reason}`);
    return { state, events };
  }

  const order: SignalOrder = {
    seq,
    t: now,
    signal,
    orderId: `ord-${Math.floor(Math.random() * 1e9)}`,
    status: "submitted",
  };
  pushOrder(state, order);
  state.stats.submitted += 1;
  events.push(`  → ORDER submitted #${seq} ${order.orderId}${state.config.dryRun ? " (dry-run)" : ""}`);
  return { state, events };
}

/** Advance one tick. May synthesize a signal + sweep fills. */
export function step(
  state: SignalBotState,
  price: number,
  now: number,
): { state: SignalBotState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Sweep submitted orders → fills. In dry-run nothing fills.
  if (!state.config.dryRun) {
    for (const o of state.signalOrders) {
      if (o.status !== "submitted") continue;
      const kind: OrderType = o.signal.side === "buy" ? "BID" : "OFFER";
      const filled = kind === "BID" ? price <= o.signal.price : price >= o.signal.price;
      if (!filled) continue;
      (o as { status: SignalOrderStatus }).status = "filled";
      (o as { filledAt?: number }).filledAt = now;
      (o as { filledPrice?: number }).filledPrice = price;
      state.stats.filled += 1;
      events.push(
        `FILL #${o.seq} ${kind} ${o.signal.quantity} ${o.signal.market} @ ${fmt(o.signal.price)} (mid=${fmt(price)})`,
      );
    }
  }

  // Poisson arrival — synthesize at most one signal per tick.
  if (Math.random() < arrivalProb(state.config.signalArrivalPerTick)) {
    const sig = synthesizeSignal(state, now);
    const res = ingestSignal(state, sig, now);
    events.push(...res.events);
  }

  return { state, events };
}

function pushOrder(state: SignalBotState, o: SignalOrder) {
  state.signalOrders.push(o);
  if (state.signalOrders.length > MAX_ORDERS) state.signalOrders.shift();
}

function round8(n: number): number { return Math.round(n * 1e8) / 1e8; }
function round(n: number, d: number): number { const p = 10 ** d; return Math.round(n * p) / p; }

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
